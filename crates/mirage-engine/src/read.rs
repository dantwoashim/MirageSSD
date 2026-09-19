use crate::copy::copy_span;
use crate::{FetchContext, MountGeneration, PageProvider};
use futures_util::future::join_all;
use mirage_backend::ObjectBackend;
use mirage_cache::{ResidentPageGuard, read_contiguous};
use mirage_index::resolve_range;
use mirage_types::MirageError;

impl<B: ObjectBackend + 'static> PageProvider<B> {
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
            // Resident pages in adjacent arena slots are served synchronously
            // by one read each; only the remaining spans take the shared miss
            // path.
            let mut guards: Vec<Option<ResidentPageGuard>> = Vec::with_capacity(chunk.len());
            for span in chunk {
                let hash = generation
                    .index
                    .page_by_ordinal(span.page_ordinal.as_u32())?
                    .plaintext_hash();
                guards.push(self.index.acquire(hash).ok().flatten());
            }
            let mut pending: Vec<usize> = Vec::new();
            let mut cursor = 0;
            while cursor < chunk.len() {
                if guards[cursor].is_none() {
                    pending.push(cursor);
                    cursor += 1;
                    continue;
                }
                let mut end = cursor + 1;
                while end < chunk.len() {
                    let (previous_span, next_span) = (chunk[end - 1], chunk[end]);
                    let (Some(next_guard), Some(previous_guard)) =
                        (guards[end].as_ref(), guards[end - 1].as_ref())
                    else {
                        break;
                    };
                    if next_span.page_offset != 0
                        || next_span.dst_offset != previous_span.dst_offset + previous_span.len
                        || previous_guard.logical_length()
                            != previous_span.page_offset + previous_span.len
                        || Some(next_guard.slot_index())
                            != previous_guard.slot_index().checked_add(1)
                    {
                        break;
                    }
                    end += 1;
                }
                let run: Vec<ResidentPageGuard> = guards[cursor..end]
                    .iter_mut()
                    .map(|guard| guard.take().expect("run is resident"))
                    .collect();
                let run_bytes = chunk[cursor..end]
                    .iter()
                    .try_fold(0usize, |sum, span| sum.checked_add(span.len as usize))
                    .ok_or_else(|| {
                        MirageError::internal_invariant("resolved destination escapes output")
                    })?;
                let dst = chunk[cursor].dst_offset as usize;
                let window = output
                    .get_mut(
                        dst..dst.checked_add(run_bytes).ok_or_else(|| {
                            MirageError::internal_invariant("resolved destination escapes output")
                        })?,
                    )
                    .ok_or_else(|| {
                        MirageError::internal_invariant("resolved destination escapes output")
                    })?;
                read_contiguous(&self.shard, &run, chunk[cursor].page_offset, window)?;
                cursor = end;
            }
            let futures = pending.iter().map(|&index| {
                let context = context.clone();
                let span = chunk[index];
                let hash = generation
                    .index
                    .page_by_ordinal(span.page_ordinal.as_u32())
                    .map(|page| page.plaintext_hash());
                async move {
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
