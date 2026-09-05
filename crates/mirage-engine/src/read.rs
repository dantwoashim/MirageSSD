use crate::copy::copy_span;
use crate::{FetchContext, MountGeneration, PageProvider};
use futures_util::future::join_all;
use mirage_backend::ObjectBackend;
use mirage_index::resolve_range;
use mirage_types::MirageError;

impl<B: ObjectBackend> PageProvider<B> {
    pub async fn read_into(
        &self,
        generation: &MountGeneration,
        file_index: u32,
        offset: u64,
        output: &mut [u8],
        context: FetchContext,
        max_fanout: usize,
    ) -> Result<usize, MirageError> {
        if max_fanout == 0 {
            return Err(MirageError::invalid_argument("read fanout is zero"));
        }
        let file = generation.file(file_index)?;
        let spans = resolve_range(file, offset, output.len())?;
        if spans.is_empty() {
            return Ok(0);
        }
        let total = spans
            .iter()
            .try_fold(0usize, |sum, span| sum.checked_add(span.len as usize))
            .ok_or_else(|| MirageError::invalid_argument("read byte count overflows"))?;
        if spans.len() == 1 {
            let span = spans[0];
            let page = generation
                .index
                .page_by_ordinal(span.page_ordinal.as_u32())?;
            let guard = self.get_or_fetch(page.plaintext_hash(), context).await?;
            copy_span(&guard, span.page_offset, &mut output[..span.len as usize])?;
            return Ok(total);
        }
        for chunk in spans.chunks(max_fanout) {
            let futures = chunk.iter().map(|span| {
                let context = context.clone();
                let hash = generation
                    .index
                    .page_by_ordinal(span.page_ordinal.as_u32())
                    .map(|page| page.plaintext_hash());
                async move {
                    let span = *span;
                    let hash = hash?;
                    let guard = self.get_or_fetch(hash, context).await?;
                    let mut bytes = vec![0; span.len as usize];
                    copy_span(&guard, span.page_offset, &mut bytes)?;
                    Ok::<_, MirageError>((span.dst_offset as usize, bytes))
                }
            });
            for result in join_all(futures).await {
                let (dst, bytes) = result?;
                let end = dst
                    .checked_add(bytes.len())
                    .filter(|end| *end <= output.len())
                    .ok_or_else(|| {
                        MirageError::internal_invariant("resolved destination escapes output")
                    })?;
                output[dst..end].copy_from_slice(&bytes);
            }
        }
        Ok(total)
    }
}
