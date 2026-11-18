//! `text` must fail loudly when a page it was asked for cannot be read (#131), and when the
//! document itself cannot be opened, e.g. a password-protected file without `--password` (#132).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_pdfcraft-cli");

/// A PDF with `n` 200×300 pt pages reading "Page 1", "Page 2", …
fn fixture(n: usize) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 200 300] >>", kids.join(" ")).into_bytes());
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for i in 0..n {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i).into_bytes());
        let body = format!("BT /F1 24 Tf 20 150 Td (Page {}) Tj ET", i + 1);
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes());
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pdfcraft-cli-text-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

#[test]
fn text_of_a_selected_page_that_cannot_be_read_fails() {
    let path = tmp("three.pdf");
    std::fs::write(&path, fixture(3)).unwrap();
    let out = Command::new(BIN).args(["text", path.to_str().unwrap(), "--page", "9"]).output().unwrap();
    assert!(!out.status.success(), "a page that cannot be read must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("page 9"), "the failing page must be named: {stderr}");
}

#[test]
fn text_of_a_readable_document_succeeds() {
    let path = tmp("ok.pdf");
    std::fs::write(&path, fixture(3)).unwrap();
    let out = Command::new(BIN).args(["text", path.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Page 1") && stdout.contains("Page 3"), "{stdout}");
}

/// `three.pdf` encrypted with the open password `openme`, via `run --script`. `tag` keeps each
/// test's files apart: tests run in parallel and would otherwise overwrite each other's.
fn protected_fixture(tag: &str) -> std::path::PathBuf {
    let src = tmp(&format!("to-protect-{tag}.pdf"));
    std::fs::write(&src, fixture(3)).unwrap();
    let out_path = tmp(&format!("protected-{tag}.pdf"));
    let script = tmp(&format!("protect-{tag}.json"));
    let steps = serde_json::json!([
        { "tool": "doc_open", "args": { "path": src } },
        { "tool": "doc_protect", "args": { "doc": 1, "open_password": "openme" } },
        { "tool": "doc_save", "args": { "doc": 1, "path": out_path } },
    ]);
    std::fs::write(&script, steps.to_string()).unwrap();
    let out = Command::new(BIN).args(["run", "--script", script.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success(), "protecting the fixture failed: {}", String::from_utf8_lossy(&out.stderr));
    out_path
}

#[test]
fn text_of_a_protected_document_without_the_password_fails() {
    let path = protected_fixture("no-password");
    let out = Command::new(BIN).args(["text", path.to_str().unwrap()]).output().unwrap();
    assert!(!out.status.success(), "a document that cannot be opened must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("password"), "the reason must be named: {stderr}");
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn text_of_a_protected_document_with_the_password_succeeds() {
    let path = protected_fixture("password");
    let out = Command::new(BIN).args(["text", path.to_str().unwrap(), "--password", "openme"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Page 1") && stdout.contains("Page 3"), "{stdout}");
}

/// `three.pdf` encrypted so that the open password `openme` doesn't allow copying, and the
/// permissions password `boss` lifts that.
fn copy_protected_fixture() -> std::path::PathBuf {
    let src = tmp("to-protect-no-copy.pdf");
    std::fs::write(&src, fixture(3)).unwrap();
    let out_path = tmp("protected-no-copy.pdf");
    let script = tmp("protect-no-copy.json");
    let steps = serde_json::json!([
        { "tool": "doc_open", "args": { "path": src } },
        { "tool": "doc_protect", "args": { "doc": 1, "open_password": "openme", "permissions_password": "boss", "copy": false } },
        { "tool": "doc_save", "args": { "doc": 1, "path": out_path } },
    ]);
    std::fs::write(&script, steps.to_string()).unwrap();
    let out = Command::new(BIN).args(["run", "--script", script.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success(), "protecting the fixture failed: {}", String::from_utf8_lossy(&out.stderr));
    out_path
}

/// `text` copies text out, so it asks what `text_extract` asks (#1645): the open password alone
/// gives no text when copying isn't allowed, the permissions password does.
#[test]
fn text_respects_a_document_that_does_not_allow_copying() {
    let path = copy_protected_fixture();
    let out = Command::new(BIN).args(["text", path.to_str().unwrap(), "--password", "openme"]).output().unwrap();
    assert!(!out.status.success(), "copying isn't allowed: {}", String::from_utf8_lossy(&out.stdout));
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("don't allow copying"), "the reason must be named: {stderr}");
    assert!(stderr.contains("--password"), "and the way in: {stderr}");
    let out = Command::new(BIN).args(["text", path.to_str().unwrap(), "--password", "boss"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Page 1") && stdout.contains("Page 3"), "{stdout}");
}

mod page_argument_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct PageDir(PathBuf);

    impl PageDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            for _ in 0..128 {
                let serial = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!("pdfcraft-page-{}-{serial}", std::process::id()));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("creating page test directory: {error}"),
                }
            }
            panic!("no unused page test directory after 128 attempts");
        }
    }

    impl Drop for PageDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn malformed_page_values_fail_before_reading_or_printing_document_text() {
        let dir = PageDir::new();
        let path = dir.0.join("three.pdf");
        let bytes = fixture(3);
        std::fs::write(&path, &bytes).unwrap();
        let missing = dir.0.join("missing.pdf");
        let invalid = [
            vec!["--page", "0"],
            vec!["--page", ""],
            vec!["--page", "bad"],
            vec!["--page", "-1"],
            vec!["--page", "1.5"],
            vec!["--page", "18446744073709551616"],
            vec!["--page"],
            vec!["--page", "--password", "unused"],
            vec!["--page", "2", "--page"],
        ];
        for input in [&path, &missing] {
            for args in &invalid {
                let out = Command::new(BIN).arg("text").arg(input).args(args).output().unwrap();
                assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
                assert!(out.stdout.is_empty(), "{args:?}: no text may be printed");
                assert_eq!(String::from_utf8(out.stderr).unwrap().trim(), "pdfcraft-cli: bad --page: expected a positive page number");
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert!(!missing.exists());
            }
        }
    }

    #[test]
    fn valid_page_values_preserve_all_page_selection_and_first_duplicate_semantics() {
        let dir = PageDir::new();
        let path = dir.0.join("three.pdf");
        let bytes = fixture(3);
        std::fs::write(&path, &bytes).unwrap();
        for (args, expected) in [
            (vec![], vec!["Page 1", "Page 2", "Page 3"]),
            (vec!["--page", "1"], vec!["Page 1"]),
            (vec!["--page", "2"], vec!["Page 2"]),
            (vec!["--page", "3"], vec!["Page 3"]),
            (vec!["--page", "2", "--page", "1"], vec!["Page 2"]),
        ] {
            let out = Command::new(BIN).arg("text").arg(&path).args(args).output().unwrap();
            assert!(out.status.success(), "{out:?}");
            assert!(out.stderr.is_empty());
            let text = String::from_utf8(out.stdout).unwrap();
            assert_eq!(text.split('').map(str::trim).collect::<Vec<_>>(), expected);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn an_option_shaped_password_is_not_parsed_as_a_page_flag() {
        let dir = PageDir::new();
        let source = dir.0.join("three.pdf");
        let bytes = fixture(3);
        std::fs::write(&source, &bytes).unwrap();
        let protected = dir.0.join("protected.pdf");
        let script = dir.0.join("protect.json");
        let steps = serde_json::json!([
            { "tool": "doc_open", "args": { "path": source } },
            { "tool": "doc_protect", "args": { "doc": 1, "open_password": "--page" } },
            { "tool": "doc_save", "args": { "doc": 1, "path": protected } },
        ]);
        std::fs::write(&script, steps.to_string()).unwrap();
        let out = Command::new(BIN).args(["run", "--script"]).arg(&script).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let protected_bytes = std::fs::read(&protected).unwrap();
        for (args, expected) in [
            (vec!["--password", "--page"], vec!["Page 1", "Page 2", "Page 3"]),
            (vec!["--page", "2", "--password", "--page"], vec!["Page 2"]),
            (vec!["--password", "--page", "--page", "2"], vec!["Page 2"]),
        ] {
            let out = Command::new(BIN).arg("text").arg(&protected).args(args).output().unwrap();
            assert!(out.status.success(), "{out:?}");
            assert!(out.stderr.is_empty());
            let text = String::from_utf8(out.stdout).unwrap();
            assert_eq!(text.split('').map(str::trim).collect::<Vec<_>>(), expected);
            assert_eq!(std::fs::read(&protected).unwrap(), protected_bytes);
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
        }
    }

    #[test]
    fn text_clips_glyphs_outside_the_crop_box() {
        let dir = PageDir::new();
        let path = dir.0.join("cropped.pdf");
        let body = "BT /F1 12 Tf 20 260 Td (Outside header) Tj 0 -110 Td (Visible body) Tj 0 -120 Td (Outside footer) Tj ET";
        let objs: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 /MediaBox [0 0 200 300] >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /CropBox [0 100 200 200] /Contents 5 0 R /Resources << /Font << /F1 3 0 R >> >> >>".to_vec(),
            format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes(),
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(o);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offsets {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        std::fs::write(&path, pdf).unwrap();

        for args in [vec![], vec!["--page", "1"]] {
            let out = Command::new(BIN).arg("text").arg(&path).args(&args).output().unwrap();
            assert!(out.status.success(), "{args:?}: {out:?}");
            assert!(out.stderr.is_empty(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "Visible body", "{args:?}");
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn ai_summary_previews_then_sends_only_confirmed_pages() {
    ai_provider_mock(false, "ai_summarize");
}

#[cfg(target_os = "macos")]
#[test]
fn ai_summary_supports_responses_endpoint_and_output() {
    ai_provider_mock(true, "ai_summarize");
}

#[cfg(target_os = "macos")]
#[test]
fn ai_ask_retrieves_follow_up_context_and_validates_citations() {
    ai_provider_mock(true, "ai_ask");
}

#[cfg(target_os = "macos")]
#[test]
fn ai_translate_sends_the_target_language_and_only_confirmed_pages() {
    ai_provider_mock(true, "ai_translate");
}

#[cfg(target_os = "macos")]
fn ai_provider_mock(responses: bool, tool: &str) {
    let ask = tool == "ai_ask";
    let translate = tool == "ai_translate";
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    use serde_json::{Value, json};

    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("Library/Application Support/PdfCraft");
    std::fs::create_dir_all(&settings).unwrap();
    let pdf = root.path().join("fixture.pdf");
    let text = String::from_utf8(fixture(3)).unwrap();
    let text = if ask { text.replace("Page 1", "Budget").replace("Page 2", "Refund").replace("Page 3", "Travel") } else { text };
    std::fs::write(&pdf, text).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let route = if responses { "/v1/responses" } else { "/v1/chat/completions" };
    let endpoint = format!("http://{}{route}", listener.local_addr().unwrap());
    let preferences = settings.join("ai-providers.json");
    let mut profile = json!({ "name": "Mock", "base_url": base_url, "model": "mock-model" });
    if responses {
        profile["api_format"] = json!("responses");
        profile["base_url"] = json!(endpoint);
    }
    let save = |active: Value| {
        std::fs::write(&preferences, json!({"active":active, "profiles":[profile]}).to_string()).unwrap();
    };
    let run = |args: Value, mark: bool| {
        let script = root.path().join("summary.json");
        let mut steps = vec![json!({"tool":"doc_open","args":{"path":pdf}})];
        if mark {
            steps.push(json!({"tool":"redact_mark","args":{"doc":1,"page":2,"whole_pages":true}}));
        }
        steps.push(json!({"tool":tool,"args":args}));
        let steps = json!(steps);
        std::fs::write(&script, steps.to_string()).unwrap();
        let output = Command::new(BIN)
            .env("HOME", root.path())
            .args(["run", "--report", "--script"])
            .arg(&script)
            .arg("--root")
            .arg(root.path())
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
        (output.status.success(), report)
    };
    let args = if ask {
        json!({"doc":1, "question":"Are there exceptions?", "previous_question":"What is the refund policy?", "previous_answer":"The policy mentions refunds."})
    } else if translate {
        json!({"doc":1,"first_page":2,"last_page":2,"target_language":"Español"})
    } else {
        json!({"doc":1,"first_page":2,"last_page":2})
    };
    save(Value::Null);
    let (ok, disabled) = run(args.clone(), false);
    assert!(!ok);
    assert!(disabled["steps"][1]["error"].as_str().unwrap().contains("Select an active provider"), "{disabled}");
    save(json!("Mock"));
    let (ok, preview) = run(args.clone(), false);
    assert!(ok, "{preview}");
    let preview = &preview["steps"][1]["result"][0];
    assert_eq!(preview["sent"], false);
    assert_eq!(preview["endpoint"], endpoint);
    if translate {
        assert_eq!(preview["target_language"], "Español");
    }

    let mut confirm = args;
    confirm["user_confirmed"] = json!(true);
    confirm["confirmed_endpoint"] = json!("http://127.0.0.1/other");
    confirm["confirmed_model"] = json!("mock-model");
    let (ok, mismatch) = run(confirm.clone(), false);
    assert!(!ok);
    assert!(mismatch["steps"][1]["error"].as_str().unwrap().contains("must match the preview"), "{mismatch}");
    confirm["confirmed_endpoint"] = json!(endpoint);
    let (ok, marked) = run(confirm.clone(), true);
    assert!(!ok);
    assert!(marked["steps"][2]["error"].as_str().unwrap().contains("unapplied redactions"), "{marked}");

    if ask {
        let mut unmatched = confirm.clone();
        unmatched["question"] = json!("Zebras?");
        unmatched.as_object_mut().unwrap().remove("previous_question");
        unmatched.as_object_mut().unwrap().remove("previous_answer");
        let (ok, no_match) = run(unmatched, false);
        assert!(ok, "{no_match}");
        assert_eq!(no_match["steps"][1]["result"][0]["sent"], false);
    }

    listener.set_nonblocking(true).unwrap();
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    listener.set_nonblocking(false).unwrap();

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            // Keep the mock bounded even if a regression prevents the request.
            listener.set_nonblocking(true).unwrap();
            let start = std::time::Instant::now();
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && start.elapsed() < Duration::from_secs(5) => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("mock provider received no request: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut read = BufReader::new(&mut socket);
            let mut line = String::new();
            read.read_line(&mut line).unwrap();
            assert_eq!(line, format!("POST {route} HTTP/1.1\r\n"));
            let mut length = None;
            loop {
                line.clear();
                assert_ne!(read.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    assert!(!name.eq_ignore_ascii_case("Authorization"), "keyless local profile");
                    if name.eq_ignore_ascii_case("Content-Length") {
                        length = Some(value.trim().parse::<usize>().unwrap());
                    }
                }
            }
            let mut body = vec![0; length.unwrap()];
            read.read_exact(&mut body).unwrap();
            let payload: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(payload["model"], "mock-model");
            let reply = if ask {
                let input: Value = serde_json::from_str(payload["input"].as_str().unwrap()).unwrap();
                assert_eq!(input["question"], "Are there exceptions?");
                assert_eq!(input["recent_turns"][0]["question"], "What is the refund policy?");
                assert_eq!(input["sources"], json!([{"source":1,"page":2,"text":"Refund"}]));
                r#"{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"The refund passage applies [S1]. [S99]"}]}]}"#
            } else if translate {
                let input: Value = serde_json::from_str(payload["input"].as_str().unwrap()).unwrap();
                assert_eq!(input, json!({"target_language":"Español", "text":"[Page 2]\nPage 2\n\n"}));
                assert!(payload["instructions"].as_str().unwrap().contains("Translate"));
                r#"{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Página 2"}]}]}"#
            } else if responses {
                assert_eq!(payload["input"], "[Page 2]\nPage 2\n\n");
                assert_eq!(payload["store"], false);
                assert!(payload["instructions"].as_str().unwrap().contains("Summarize"));
                assert!(payload.get("messages").is_none());
                r#"{"status":"completed","output":[{"type":"reasoning"},{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Intermediate text"}]},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"A summary of page two."}]}]}"#
            } else {
                assert_eq!(payload["messages"][1]["content"], "[Page 2]\nPage 2\n\n");
                r#"{"choices":[{"message":{"content":"A summary of page two."},"finish_reason":"stop"}]}"#
            };
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        });
        let (ok, result) = run(confirm, false);
        server.join().unwrap();
        assert!(ok, "{result}");
        assert_eq!(result["steps"][1]["result"][0]["sent"], true);
        let result = &result["steps"][1]["result"][0];
        if ask {
            assert_eq!(result["answer"].as_str().unwrap().trim(), "The refund passage applies [Page 2].");
            assert_eq!(result["citations"], json!([{"page":2,"excerpt":"Refund"}]));
        } else if translate {
            assert_eq!(result["translation"], "Página 2");
        } else {
            assert_eq!(result["summary"], "A summary of page two.");
        }
    });
}
