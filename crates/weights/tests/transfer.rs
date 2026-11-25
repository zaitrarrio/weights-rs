//! Transfers against a local HTTP server that serves ranges (and can drop
//! requests or ignore ranges), and local copies.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use weights::{Item, Options, Source, fetch, plan_items, tree_items};

fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Mode {
    Ranges,
    /// Every third request fails with a 503.
    Flaky,
    /// Ignores `Range` and always sends the whole body with 200.
    NoRanges,
    /// Sends half of each range, then closes (a broken connection).
    Truncate,
}

struct Server {
    url: String,
    requests: Arc<AtomicUsize>,
}

fn serve(files: Vec<(&'static str, Vec<u8>)>, mode: Mode) -> Server {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let files: Vec<(String, Arc<Vec<u8>>)> = files
        .into_iter()
        .map(|(n, d)| (format!("/{n}"), Arc::new(d)))
        .collect();
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let n = count.fetch_add(1, Ordering::SeqCst) + 1;
            let path = req.url().split('?').next().unwrap().to_string();
            let Some((_, body)) = files.iter().find(|(p, _)| *p == path) else {
                let _ = req.respond(tiny_http::Response::empty(404));
                continue;
            };
            if matches!(mode, Mode::Flaky) && n.is_multiple_of(3) {
                let _ = req.respond(tiny_http::Response::empty(503));
                continue;
            }
            let range = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Range"))
                .map(|h| h.value.as_str().to_string());
            match (mode, range) {
                (Mode::NoRanges, _) | (_, None) => {
                    let _ = req.respond(tiny_http::Response::from_data(body.to_vec()));
                }
                (_, Some(r)) => {
                    let (a, b) = r.trim_start_matches("bytes=").split_once('-').unwrap();
                    let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                    let b = b.min(body.len() - 1);
                    let mut slice = body[a..=b].to_vec();
                    if matches!(mode, Mode::Truncate) && slice.len() > 1 && n.is_multiple_of(2) {
                        // Promise the whole range, send half: the client sees a short body.
                        let full = slice.len();
                        slice.truncate(full / 2);
                        let header = format!("bytes {a}-{b}/{}", body.len());
                        let resp = tiny_http::Response::new(
                            tiny_http::StatusCode(206),
                            vec![tiny_http::Header::from_bytes("Content-Range", header).unwrap()],
                            std::io::Cursor::new(slice),
                            Some(full),
                            None,
                        );
                        let _ = req.respond(resp);
                        continue;
                    }
                    let header = format!("bytes {a}-{b}/{}", body.len());
                    let resp = tiny_http::Response::from_data(slice)
                        .with_status_code(206)
                        .with_header(
                            tiny_http::Header::from_bytes("Content-Range", header).unwrap(),
                        );
                    let _ = req.respond(resp);
                }
            }
        }
    });
    Server { url, requests }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("weights-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn opts(chunk: u64, jobs: usize) -> Options {
    Options {
        jobs,
        chunk,
        retries: 6,
        progress_s: 0,
        read_timeout_s: 2,
    }
}

#[test]
fn ranged_download_of_several_files_is_exact() {
    let (a, b, c) = (data(3_000_017, 1), data(70_001, 2), Vec::new());
    let s = serve(
        vec![
            ("a.bin", a.clone()),
            ("b.bin", b.clone()),
            ("c.bin", c.clone()),
        ],
        Mode::Ranges,
    );
    let dir = tmp("ranged");
    let items = vec![
        Item {
            src: Source::Url(format!("{}/a.bin?X-Amz-Signature=secret", s.url)),
            dest: dir.join("x/a.bin"),
            size: None,
        },
        Item {
            src: Source::Url(format!("{}/b.bin", s.url)),
            dest: dir.join("b.bin"),
            size: Some(b.len() as u64),
        },
        Item {
            src: Source::Url(format!("{}/c.bin", s.url)),
            dest: dir.join("c.bin"),
            size: Some(0),
        },
    ];
    let st = fetch(&items, &opts(256 << 10, 8)).unwrap();
    assert_eq!(std::fs::read(dir.join("x/a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), b);
    assert_eq!(std::fs::read(dir.join("c.bin")).unwrap(), c);
    assert_eq!(
        (st.files, st.skipped, st.moved_bytes),
        (3, 0, (a.len() + b.len()) as u64)
    );
    assert!(!dir.join("x/a.bin.part").exists() && !dir.join("x/a.bin.part.done").exists());
    // A second run finds every file complete and requests nothing but the size probe.
    let before = s.requests.load(Ordering::SeqCst);
    let st = fetch(&items, &opts(256 << 10, 8)).unwrap();
    assert_eq!((st.skipped, st.moved_bytes), (3, 0));
    assert_eq!(
        s.requests.load(Ordering::SeqCst) - before,
        1,
        "only a.bin's size probe"
    );
}

#[test]
fn failed_and_truncated_ranges_are_retried() {
    let a = data(2_000_003, 3);
    for mode in [Mode::Flaky, Mode::Truncate] {
        let s = serve(vec![("a.bin", a.clone())], mode);
        let dir = tmp(if matches!(mode, Mode::Flaky) {
            "flaky"
        } else {
            "trunc"
        });
        let items = vec![Item {
            src: Source::Url(format!("{}/a.bin", s.url)),
            dest: dir.join("a.bin"),
            size: Some(a.len() as u64),
        }];
        let st = fetch(&items, &opts(100_000, 4)).unwrap();
        assert_eq!(std::fs::read(dir.join("a.bin")).unwrap(), a);
        assert_eq!(
            st.moved_bytes,
            a.len() as u64,
            "bytes of failed attempts are not counted"
        );
    }
}

#[test]
fn a_server_without_ranges_is_read_in_one_stream() {
    let a = data(500_000, 4);
    let s = serve(vec![("a.bin", a.clone())], Mode::NoRanges);
    let dir = tmp("noranges");
    let items = vec![Item {
        src: Source::Url(format!("{}/a.bin", s.url)),
        dest: dir.join("a.bin"),
        size: None,
    }];
    fetch(&items, &opts(64 << 10, 8)).unwrap();
    assert_eq!(std::fs::read(dir.join("a.bin")).unwrap(), a);
}

#[test]
fn an_interrupted_transfer_resumes_only_the_missing_ranges() {
    let a = data(1_000_000, 5);
    let s = serve(vec![("a.bin", a.clone())], Mode::Ranges);
    let dir = tmp("resume");
    let dest = dir.join("a.bin");
    // As left by a stopped run: ranges 0, 2 and 3 of ten written and recorded.
    let chunk = 100_000usize;
    let mut part = vec![0u8; a.len()];
    for i in [0usize, 2, 3] {
        part[i * chunk..(i + 1) * chunk].copy_from_slice(&a[i * chunk..(i + 1) * chunk]);
    }
    std::fs::write(dir.join("a.bin.part"), &part).unwrap();
    std::fs::write(dir.join("a.bin.part.done"), "0\n2\n3\n").unwrap();
    let items = vec![Item {
        src: Source::Url(format!("{}/a.bin", s.url)),
        dest: dest.clone(),
        size: Some(a.len() as u64),
    }];
    let st = fetch(&items, &opts(chunk as u64, 3)).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), a);
    assert_eq!(st.moved_bytes, 7 * chunk as u64);
    assert_eq!(
        s.requests.load(Ordering::SeqCst),
        7,
        "one request per missing range"
    );
}

#[test]
fn local_trees_copy_in_ranges_and_stay_under_their_root() {
    let base = tmp("copy");
    let (vol, local) = (base.join("vol"), base.join("local"));
    std::fs::create_dir_all(vol.join("w/t/sub")).unwrap();
    let big = data(1_234_567, 6);
    std::fs::write(vol.join("w/t/big.bin"), &big).unwrap();
    std::fs::write(vol.join("w/t/sub/small.bin"), b"abc").unwrap();
    std::fs::write(base.join("outside.bin"), b"x").unwrap();
    let items = tree_items(&[vol.join("w/t")], &vol, &local).unwrap();
    assert_eq!(items.len(), 2);
    let st = fetch(&items, &opts(100_000, 4)).unwrap();
    assert_eq!(std::fs::read(local.join("w/t/big.bin")).unwrap(), big);
    assert_eq!(
        std::fs::read(local.join("w/t/sub/small.bin")).unwrap(),
        b"abc"
    );
    assert_eq!(st.moved_bytes, big.len() as u64 + 3);
    assert_eq!(
        fetch(&items, &opts(100_000, 4)).unwrap().moved_bytes,
        0,
        "a second copy moves nothing"
    );
    assert!(tree_items(&[base.join("outside.bin")], &vol, &local).is_err());
    assert!(
        tree_items(&[vol.join("w/../../outside.bin")], &vol, &local).is_err(),
        "`..` cannot leave the root"
    );
}

#[test]
fn plans_parse_with_and_without_sizes() {
    let p = plan_items(
        "# comment\nhttps://r2.example/a?sig=1 123 weights/a.bin\n\n/vol/b.bin /abs/b.bin\n",
        Some(Path::new("/w")),
    )
    .unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].src, Source::Url("https://r2.example/a?sig=1".into()));
    assert_eq!(
        (p[0].size, p[0].dest.clone()),
        (Some(123), PathBuf::from("/w/weights/a.bin"))
    );
    assert_eq!(
        (p[1].src.clone(), p[1].size, p[1].dest.clone()),
        (
            Source::Path("/vol/b.bin".into()),
            None,
            PathBuf::from("/abs/b.bin")
        )
    );
    assert!(plan_items("a b c d\n", None).is_err());
    assert_eq!(
        weights::redact("https://x/y?X-Amz-Signature=s"),
        "https://x/y"
    );
}

#[test]
fn no_proxy_matching() {
    use weights::{bypass, host_of};
    let e: Vec<String> = ["localhost", "127.0.0.0/8", ".internal.example", "::1"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(host_of("http://127.0.0.1:8080/a?b=c"), "127.0.0.1");
    assert_eq!(host_of("https://user@R2.example.com/x"), "r2.example.com");
    assert_eq!(host_of("http://[::1]:9/x"), "::1");
    assert!(bypass(&e, host_of("http://127.4.5.6/x")));
    assert!(bypass(&e, host_of("http://localhost:1/x")));
    assert!(bypass(&e, host_of("https://a.internal.example/x")));
    assert!(bypass(&e, host_of("http://[::1]/x")));
    assert!(!bypass(&e, host_of("https://r2.cloudflarestorage.com/x")));
    assert!(!bypass(&e, host_of("http://10.0.0.1/x")));
}

#[test]
fn errors_never_carry_a_presigned_signature() {
    let s = serve(vec![], Mode::Ranges);
    let dir = tmp("redact");
    let items = vec![Item {
        src: Source::Url(format!("{}/missing.bin?X-Amz-Signature=topsecret", s.url)),
        dest: dir.join("m"),
        size: None,
    }];
    let e = format!("{:#}", fetch(&items, &opts(1 << 20, 2)).unwrap_err());
    assert!(e.contains("404") && !e.contains("topsecret"), "{e}");
}
