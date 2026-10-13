//! Actual loopback HTTP range transport with controlled response delay and a
//! SHARED bandwidth limiter. The backend serves an unchanged encrypted object.
use super::*;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

const RATE: f64 = (20 * 1024 * 1024) as f64;
const GRANULE: usize = 65536;

fn append_leaves(tree: &Tree, out: &mut Vec<u8>) {
    match &tree.children {
        Some((left, right)) => {
            append_leaves(left, out);
            append_leaves(right, out);
        }
        None => out.extend_from_slice(&tree.cv),
    }
}

fn from_leaves(cvs: &mut std::slice::ChunksExact<'_, u8>, start: usize, length: usize) -> Tree {
    if length <= GRANULE {
        Tree {
            start,
            length,
            cv: cvs.next().unwrap().try_into().unwrap(),
            children: None,
        }
    } else {
        let split = left_subtree_len(length as u64) as usize;
        let left = Box::new(from_leaves(cvs, start, split));
        let right = Box::new(from_leaves(cvs, start + split, length - split));
        Tree {
            start,
            length,
            cv: merge_subtrees_non_root(&left.cv, &right.cv, Mode::Hash),
            children: Some((left, right)),
        }
    }
}

fn parse_catalog(bytes: &[u8], aad: EncryptedFrameAad) -> ([u8; PREFIX], Tree) {
    // This transport fixture is intentionally fixed at 4 MiB / 64 KiB.
    assert_eq!(bytes.len(), PREFIX + FRAME / GRANULE * 32);
    let header = bytes[..PREFIX].try_into().unwrap();
    let mut cvs = bytes[PREFIX..].chunks_exact(32);
    let tree = from_leaves(&mut cvs, 0, FRAME);
    assert!(cvs.next().is_none());
    let (left, right) = tree.children.as_ref().unwrap();
    assert_eq!(
        merge_subtrees_root(&left.cv, &right.cv, Mode::Hash).as_bytes(),
        aad.plaintext_hash.as_bytes()
    );
    (header, tree)
}

fn get(addr: SocketAddr, path: &str, range: Option<(usize, usize)>) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some((start, length)) = range {
        request.push_str(&format!("Range: bytes={start}-{}\r\n", start + length - 1));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = Vec::new();
    stream
        .take(8 * 1024 * 1024)
        .read_to_end(&mut response)
        .unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let header = std::str::from_utf8(&response[..split]).unwrap();
    assert!(header.starts_with(if range.is_some() {
        "HTTP/1.1 206"
    } else {
        "HTTP/1.1 200"
    }));
    let length: usize = header
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .unwrap()
        .parse()
        .unwrap();
    if let Some((start, requested)) = range {
        assert_eq!(length, requested);
        assert!(header.contains(&format!(
            "Content-Range: bytes {start}-{}",
            start + requested - 1
        )));
    }
    let body = response[split + 4..].to_vec();
    assert_eq!(body.len(), length);
    body
}

pub(super) fn run() -> serde_json::Value {
    let key = RepositoryKey::from_bytes([0x41; 32]);
    let (plain, encoded, id) = fixture(FRAME, 93822, &key);
    assert_eq!(
        decode_encrypted_frame(&key, &encoded, id.aad).unwrap(),
        plain
    );
    let original_tree = Tree::build(&plain, 0, GRANULE);
    let mut catalog = encoded[..PREFIX].to_vec();
    append_leaves(&original_tree, &mut catalog);
    let (cached_header, cached_tree) = parse_catalog(&catalog, id.aad);
    let content = Arc::new(encoded);
    let metadata = Arc::new(catalog);
    let latency_ms = Arc::new(AtomicU64::new(0));
    let bandwidth_finish = Arc::new(Mutex::new(Instant::now()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let delay = Arc::clone(&latency_ms);
    let blob = Arc::clone(&content);
    let meta = Arc::clone(&metadata);
    let limit = Arc::clone(&bandwidth_finish);
    let server = std::thread::spawn(move || {
        let mut workers = Vec::new();
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192);
            }
            let request = String::from_utf8(request).unwrap();
            if request.starts_with("GET /stop ") {
                break;
            }
            let bytes = if request.starts_with("GET /index ") {
                Arc::clone(&meta)
            } else {
                assert!(request.starts_with("GET /blob "));
                Arc::clone(&blob)
            };
            let range = request
                .lines()
                .find_map(|l| l.strip_prefix("Range: bytes="))
                .map(|s| {
                    let (a, b) = s.split_once('-').unwrap();
                    (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap() + 1)
                });
            let (start, end) = range.unwrap_or((0, bytes.len()));
            assert!(start < end && end <= bytes.len());
            let latency = delay.load(Ordering::Relaxed);
            let limiter = Arc::clone(&limit);
            workers.push(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(latency));
                let mut header = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    if range.is_some() {
                        "206 Partial Content"
                    } else {
                        "200 OK"
                    },
                    end - start
                );
                if range.is_some() {
                    header.push_str(&format!(
                        "Content-Range: bytes {start}-{}/{}\r\n",
                        end - 1,
                        bytes.len()
                    ));
                }
                header.push_str("\r\n");
                stream.write_all(header.as_bytes()).unwrap();
                for chunk in bytes[start..end].chunks(64 << 10) {
                    let ready = {
                        let mut finish = limiter.lock().unwrap();
                        *finish = (*finish).max(Instant::now())
                            + Duration::from_secs_f64(chunk.len() as f64 / RATE);
                        *finish
                    };
                    if let Some(wait) = ready.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                    stream.write_all(chunk).unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let mut rows = Vec::new();
    for delay_ms in [0_u64, 40, 200, 2000] {
        latency_ms.store(delay_ms, Ordering::Relaxed);
        let rounds = if delay_ms == 2000 { 2 } else { 4 };
        for round in 0..rounds {
            for position in 0..4 {
                let variant = (round + position) % 4;
                let offset = (round * 7919 % (FRAME / 4096)) * 4096;
                let sector_start = offset / GRANULE * GRANULE;
                let started = Instant::now();
                let output = if variant == 0 {
                    let response = get(addr, "/blob", Some((0, content.len())));
                    decode_encrypted_frame(&key, &response, id.aad).unwrap()[offset..offset + 4096]
                        .to_vec()
                } else if variant == 1 {
                    let p = proofs(Some(&cached_tree), offset, 4096, GRANULE);
                    read(
                        &key,
                        id.aad,
                        GRANULE,
                        offset,
                        4096,
                        &cached_header,
                        &p,
                        |start, size| Ok(get(addr, "/blob", Some((start, size)))),
                    )
                    .unwrap()
                } else {
                    let (m, c) = if variant == 2 {
                        let m = get(addr, "/index", None);
                        let c = get(addr, "/blob", Some((PREFIX + sector_start, GRANULE)));
                        (m, c)
                    } else {
                        std::thread::scope(|s| {
                            let m = s.spawn(|| get(addr, "/index", None));
                            let c = get(addr, "/blob", Some((PREFIX + sector_start, GRANULE)));
                            (m.join().unwrap(), c)
                        })
                    };
                    let (h, t) = parse_catalog(&m, id.aad);
                    let p = proofs(Some(&t), offset, 4096, GRANULE);
                    read(
                        &key,
                        id.aad,
                        GRANULE,
                        offset,
                        4096,
                        &h,
                        &p,
                        |start, size| {
                            assert_eq!((start, size), (PREFIX + sector_start, GRANULE));
                            Ok(c.clone())
                        },
                    )
                    .unwrap()
                };
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(output, plain[offset..offset + 4096]);
                rows.push(json!({"server_delay_ms":delay_ms,"round":round,
                    "variant":(["whole_frame_one_request","proof_cached_catalog","proof_serial_catalog","proof_parallel_catalog"][variant]),
                    "elapsed_ms":elapsed_ms,"http_requests":if variant<2 {1}else{2},
                    "response_body_bytes":if variant==0{content.len()}else{GRANULE+if variant>1{metadata.len()}else{0}}}));
            }
        }
        eprintln!("Completed {rounds} paired HTTP rounds with {delay_ms} ms server delay");
    }
    TcpStream::connect(addr)
        .unwrap()
        .write_all(b"GET /stop HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    server.join().unwrap();
    json!({"kind":"actual_loopback_HTTP_ranges_with_shared_20_MiB_s_body_limiter",
        "fixture":"Unchanged actual Mirage 4 MiB encrypted frame; exact 4 KiB returned bytes verified",
        "body_rate_limit_MiB_s":20,"catalog_bytes":metadata.len(),"rows":rows,
        "scope":"Controlled in-process loopback source, no TLS/Drive/WinFsp. Delay is artificial. OS scheduling adds noise. Counts exclude HTTP/TCP headers and initial catalog generation. Two rounds at 2 seconds are illustrative, not a tail-latency study."})
}
