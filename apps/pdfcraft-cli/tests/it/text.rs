//! `text` must fail loudly when a page it was asked for cannot be read (#131), and when the
//! document itself cannot be opened, e.g. a password-protected file without `--password` (#132).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_pdfcraft-cli");

/// A PDF with `n` 200×300 pt pages reading "Page 1", "Page 2", …
fn fixture(n: usize) -> Vec<u8> {
    fixture_streams((1..=n).map(|i| format!("BT /F1 24 Tf 20 150 Td (Page {i}) Tj ET")).collect())
}

fn fixture_streams(streams: Vec<String>) -> Vec<u8> {
    let n = streams.len();
    let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 200 300] >>", kids.join(" ")).into_bytes());
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for (i, body) in streams.into_iter().enumerate() {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i).into_bytes());
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
fn ai_ask_across_selected_pdfs_binds_scope_and_cites_each_file() {
    use std::net::TcpListener;

    use serde_json::{Value, json};

    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let paths = ["old/paper.pdf", "new/paper.pdf", "excluded.pdf"];
    let statements = ["Sample size was 30.", "Sample size was 60.", "Sample size was 99."];
    std::fs::create_dir_all(root.join("old")).unwrap();
    std::fs::create_dir_all(root.join("new")).unwrap();
    let pdf = |text| fixture_streams(vec![format!("BT /F1 12 Tf 20 220 Td ({text}) Tj ET")]);
    for (path, text) in paths.into_iter().zip(statements) {
        std::fs::write(root.join(path), pdf(text)).unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = write_ai_test_profile(root, &listener);
    let run = |arguments: Value| {
        let mut steps: Vec<_> = paths.iter().map(|path| json!({"tool":"doc_open", "args":{"path":path}})).collect();
        steps.push(json!({"tool":"ai_ask", "args":arguments}));
        run_ai_script(root, json!(steps))
    };
    let mut args = json!({"documents":[{"doc":1},{"doc":2}],"question":"What was the sample size?"});
    let (ok, preview) = run(args.clone());
    assert!(ok, "{preview}");
    let preview = &preview["steps"][3]["result"][0];
    assert_eq!(preview["sent"], false);
    assert_eq!(preview["documents"][0]["document"], "paper.pdf #1");
    assert_eq!(preview["documents"][1]["document"], "paper.pdf #2");
    args["user_confirmed"] = json!(true);
    args["confirmed_endpoint"] = json!(endpoint);
    args["confirmed_model"] = json!("mock-model");
    args["confirmed_scope"] = preview["scope_token"].clone();

    let mut wrong = args.clone();
    wrong["documents"][1]["doc"] = json!(3);
    let (ok, report) = run(wrong);
    assert!(!ok && report["steps"][3]["error"].as_str().unwrap().contains("scope token"), "{report}");
    std::fs::write(root.join(paths[1]), pdf("Sample size was 70.")).unwrap();
    let (ok, report) = run(args.clone());
    assert!(!ok && report["steps"][3]["error"].as_str().unwrap().contains("scope token"), "{report}");
    std::fs::write(root.join(paths[1]), pdf(statements[1])).unwrap();

    let mut paraphrase = args.clone();
    paraphrase["question"] = json!("How big was the cohort?");
    let (ok, report) = run(paraphrase);
    assert!(ok, "{report}");
    assert_eq!(report["steps"][3]["result"][0]["sent"], false, "the keyword index should decline an unsupported paraphrase");
    listener.set_nonblocking(true).unwrap();
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (socket, body) = ai_mock_request(&listener, "POST /v1/chat/completions");
            let payload: Value = serde_json::from_slice(&body).unwrap();
            let instruction = payload["messages"][0]["content"].as_str().unwrap();
            assert!(instruction.contains("When documents disagree"));
            let input: Value = serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(
                input["sources"],
                json!([
                    {"source":1,"document":"paper.pdf #1","page":1,"text":statements[0]},
                    {"source":2,"document":"paper.pdf #2","page":1,"text":statements[1]}
                ])
            );
            ai_mock_reply(socket, r#"{"choices":[{"message":{"content":"One reports 30 [S1], the other 60 [S2]. [S999]"},"finish_reason":"stop"}]}"#);
        });
        let (ok, report) = run(args);
        server.join().unwrap();
        assert!(ok, "{report}");
        let result = &report["steps"][3]["result"][0];
        assert_eq!(result["sent"], true);
        assert_eq!(result["answer"].as_str().unwrap().trim(), "One reports 30 [paper.pdf #1, page 1], the other 60 [paper.pdf #2, page 1].");
        let cites = result["citations"].as_array().unwrap();
        assert_eq!(cites.len(), 2);
        for (i, cite) in cites.iter().enumerate() {
            assert_eq!(cite["location"]["doc"], i + 1);
            assert_eq!(cite["location"]["revision"], preview["documents"][i]["revision"]);
            assert_eq!(cite["excerpt"], statements[i]);
            assert_eq!(cite["location"]["rects"].as_array().unwrap().len(), 1);
        }
    });
}

#[cfg(target_os = "macos")]
#[test]
fn ai_ask_text_selection_sends_only_the_selected_text() {
    use std::net::TcpListener;

    use serde_json::{Value, json};

    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let pdf = fixture_streams(vec!["BT /F1 12 Tf 20 220 Td (Selected passage.) Tj 0 -40 Td (Private note.) Tj ET".into()]);
    std::fs::write(root.join("selection.pdf"), pdf).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = write_ai_test_profile(root, &listener);
    let run = |args| {
        run_ai_script(
            root,
            json!([
                {"tool":"doc_open","args":{"path":"selection.pdf"}}, {"tool":"ai_ask","args":args}
            ]),
        )
    };
    let mut args = json!({"doc":1,"selection":{"page":1,"rect":[10,65,190,95]},"question":"What does this mean?"});
    let (ok, report) = run(args.clone());
    assert!(ok, "{report}");
    args["user_confirmed"] = json!(true);
    args["confirmed_endpoint"] = json!(endpoint);
    args["confirmed_model"] = json!("mock-model");
    args["confirmed_scope"] = report["steps"][1]["result"][0]["scope_token"].clone();

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (socket, body) = ai_mock_request(&listener, "POST /v1/chat/completions");
            let payload: Value = serde_json::from_slice(&body).unwrap();
            let input: Value = serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(input["sources"], json!([{"source":1,"document":"selection.pdf","page":1,"text":"Selected passage."}]));
            assert_eq!(input["recent_turns"], json!([]));
            ai_mock_reply(socket, r#"{"choices":[{"message":{"content":"The selected words [S1]."},"finish_reason":"stop"}]}"#);
        });
        let (ok, report) = run(args);
        server.join().unwrap();
        assert!(ok, "{report}");
        assert_eq!(report["steps"][1]["result"][0]["citations"][0]["excerpt"], "Selected passage.");
    });
}

#[cfg(target_os = "macos")]
fn scan_fixture() -> std::sync::Arc<Vec<u8>> {
    use pdfcraft_engine::{ImageResolution, Session};
    use pdfcraft_render::{PageRenderer, RenderRequest};

    let source =
        fixture_streams(vec!["BT /F1 18 Tf 18 225 Td (Copper harvest) Tj 0 -36 Td (Yield was 47 tons) Tj 0 -120 Td (Private note) Tj ET".into()]);
    let raster = PageRenderer::new(source.into(), Default::default()).render(RenderRequest { scale: 3.0, ..Default::default() });
    assert!(raster.error.is_none());
    let png = pdfcraft_engine::export::encode_png(raster.width, raster.height, &raster.rgba).unwrap();
    Session::new().create_from_images_with_resolution(&[("scan.png".into(), png)], ImageResolution::Dpi(216.0)).unwrap()
}

#[cfg(target_os = "macos")]
#[test]
fn ai_scanned_pdf_asks_and_translates_with_local_ocr_without_editing() {
    use std::net::TcpListener;

    use serde_json::{Value, json};

    if !pdfcraft_engine::ocr::engines_status().iter().any(|engine| engine.id == "ocrs" && engine.available) {
        assert!(std::env::var_os("PDFCRAFT_MODELS").is_none(), "configured ocrs models were unavailable");
        eprintln!("Set PDFCRAFT_MODELS to run the image-only PDF regression.");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let bytes = scan_fixture();
    std::fs::write(root.join("scan.pdf"), bytes.as_slice()).unwrap();
    std::fs::write(root.join("digital.pdf"), fixture_streams(vec!["BT /F1 18 Tf 18 225 Td (Copper benchmark) Tj ET".into()])).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = write_ai_test_profile(root, &listener);
    let run = |tool, args| {
        run_ai_script(
            root,
            json!([
                {"tool":"doc_open", "args":{"path":"scan.pdf"}},
                {"tool":"doc_open", "args":{"path":"digital.pdf"}},
                {"tool":tool, "args":args}
            ]),
        )
    };
    let confirm = |tool, mut args: Value| {
        let (ok, report) = run(tool, args.clone());
        assert!(ok, "{report}");
        let preview = &report["steps"][2]["result"][0];
        assert_eq!(preview["sent"], false);
        args["user_confirmed"] = json!(true);
        args["confirmed_endpoint"] = json!(endpoint);
        args["confirmed_model"] = json!("mock-model");
        args["confirmed_scope"] = preview["scope_token"].clone();
        args
    };
    let ask = confirm(
        "ai_ask",
        json!({
            "documents":[{"doc":1},{"doc":2}],"question":"What was the copper yield?", "ocr_language":"en","ocr_engine":"ocrs"
        }),
    );
    let translate = confirm(
        "ai_translate",
        json!({
            "doc":1,"selection":{"page":1,"rect":[10,45,195,145]},"target_language":"French","ocr_language":"en","ocr_engine":"ocrs"
        }),
    );
    let mut changed = translate.clone();
    changed["selection"]["rect"] = json!([10, 150, 195, 265]);
    let (ok, report) = run("ai_translate", changed);
    assert!(!ok && report["steps"][2]["error"].as_str().unwrap().contains("scope token"), "{report}");
    listener.set_nonblocking(true).unwrap();
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (socket, body) = ai_mock_request(&listener, "POST /v1/chat/completions");
            let payload: Value = serde_json::from_slice(&body).unwrap();
            let input: Value = serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
            let sources = input["sources"].as_array().unwrap();
            assert!(sources.iter().any(|s| s["document"] == "digital.pdf" && s["text"] == "Copper benchmark"), "{input}");
            let scan = sources.iter().find(|s| s["document"] == "scan.pdf").unwrap();
            assert!(scan["text"].as_str().unwrap().contains("Yield was 47 tons"), "{input}");
            let answer = format!("The yield was 47 tons [S{}].", scan["source"]);
            ai_mock_reply(socket, &json!({"choices":[{"message":{"content":answer},"finish_reason":"stop"}]}).to_string());

            let (socket, body) = ai_mock_request(&listener, "POST /v1/chat/completions");
            let payload: Value = serde_json::from_slice(&body).unwrap();
            let input: Value = serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(input["target_language"], "French");
            let text = input["text"].as_str().unwrap();
            assert!(text.contains("Copper harvest") && text.contains("47 tons") && !text.contains("Private"), "{input}");
            ai_mock_reply(socket, r#"{"choices":[{"message":{"content":"Le rendement était de 47 tonnes."},"finish_reason":"stop"}]}"#);
        });
        let (ok, report) = run("ai_ask", ask);
        assert!(ok, "{report}");
        let result = &report["steps"][2]["result"][0];
        assert_eq!(result["ocr_report"]["pages_read"], 1, "{result}");
        assert_eq!(result["citations"][0]["location"]["doc"], 1);
        assert!(result["citations"][0]["location"]["rects"].as_array().is_some_and(|rects| rects.len() >= 2));
        let (ok, report) = run("ai_translate", translate);
        assert!(ok, "{report}");
        let result = &report["steps"][2]["result"][0];
        assert_eq!(result["translation"], "Le rendement était de 47 tonnes.");
        assert_eq!(result["ocr_report"]["pages_read"], 1);
        let passage = &result["source_passages"][0];
        assert_eq!(passage["page"], 1);
        assert!(passage["text"].as_str().unwrap().contains("47 tons"));
        assert!(passage["rects"].as_array().unwrap().iter().all(|rect| rect[1].as_f64().unwrap() >= 45.0 && rect[3].as_f64().unwrap() <= 145.0));
        server.join().unwrap();
    });
    assert_eq!(std::fs::read(root.join("scan.pdf")).unwrap(), *bytes);
}

#[cfg(target_os = "macos")]
#[test]
fn ai_figure_sends_only_the_confirmed_crop_to_local_vision() {
    use std::net::TcpListener;

    use base64::Engine as _;
    use serde_json::{Value, json};

    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::write(root.join("graph.pdf"), fixture_streams(vec!["1 0 0 rg 10 195 65 60 re f 0 0 1 rg 100 195 80 60 re f".into()])).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = write_ai_test_profile(root, &listener);
    let run = |args: Value| {
        run_ai_script(
            root,
            json!([
                {"tool":"doc_open","args":{"path":"graph.pdf"}},
                {"tool":"ai_ask","args":args}
            ]),
        )
    };
    let mut args = json!({
        "doc":1,"selection":{"page":1,"rect":[5,40,85,110]},"question":"What color is the rectangle?",
        "ocr_language":"en","ocr_engine":"ocrs","vision":true
    });
    let (ok, report) = run(args.clone());
    assert!(ok, "{report}");
    args["user_confirmed"] = json!(true);
    args["confirmed_endpoint"] = json!(endpoint);
    args["confirmed_model"] = json!("mock-model");
    args["confirmed_scope"] = report["steps"][1]["result"][0]["scope_token"].clone();
    let (ok, report) = run(args.clone());
    assert!(!ok && report["steps"][1]["error"].as_str().unwrap().contains("local llama.cpp vision profile"), "{report}");
    let profile = root.join("Library/Application Support/PdfCraft/ai-providers.json");
    let mut profiles: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
    profiles["profiles"][0]["llama_cpp"] = json!(true);
    std::fs::write(profile, profiles.to_string()).unwrap();

    let (ok, report) = run_ai_script(
        root,
        json!([
            {"tool":"doc_open","args":{"path":"graph.pdf"}},
            {"tool":"redact_mark","args":{"doc":1,"page":1,"rect":[20,50,80,100]}},
            {"tool":"doc_save","args":{"doc":1,"path":"marked.pdf"}}
        ]),
    );
    assert!(ok, "{report}");
    let with_redaction = |args: Value| {
        run_ai_script(
            root,
            json!([
                {"tool":"doc_open","args":{"path":"marked.pdf"}},
                {"tool":"ai_ask","args":args}
            ]),
        )
    };
    let mut redacted_args = args.clone();
    redacted_args["user_confirmed"] = json!(false);
    let (ok, report) = with_redaction(redacted_args.clone());
    assert!(ok, "{report}");
    redacted_args["user_confirmed"] = json!(true);
    redacted_args["confirmed_scope"] = report["steps"][1]["result"][0]["scope_token"].clone();
    let (ok, report) = with_redaction(redacted_args);
    assert!(!ok && report["steps"][1]["error"].as_str().unwrap().contains("unapplied redactions"), "{report}");
    listener.set_nonblocking(true).unwrap();
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (socket, body) = ai_mock_request(&listener, "POST /v1/chat/completions");
            let payload: Value = serde_json::from_slice(&body).unwrap();
            let content = &payload["messages"][1]["content"];
            assert_eq!(content[1]["text"], "What color is the rectangle?");
            let url = content[0]["image_url"]["url"].as_str().unwrap();
            let png = base64::engine::general_purpose::STANDARD.decode(url.strip_prefix("data:image/png;base64,").unwrap()).unwrap();
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 240);
            assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 210);
            ai_mock_reply(socket, r#"{"choices":[{"message":{"content":"The rectangle is red."},"finish_reason":"stop"}]}"#);
        });
        let (ok, report) = run(args);
        server.join().unwrap();
        assert!(ok, "{report}");
        let result = &report["steps"][1]["result"][0];
        assert_eq!(result["answer"], "The rectangle is red.");
        assert_eq!(result["citations"][0]["location"]["rects"], json!([[5.0, 40.0, 85.0, 110.0]]));
    });
}

#[cfg(target_os = "macos")]
fn write_ai_test_profile(root: &std::path::Path, listener: &std::net::TcpListener) -> String {
    let settings = root.join("Library/Application Support/PdfCraft");
    std::fs::create_dir_all(&settings).unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let preferences = serde_json::json!({"active":"Mock","profiles":[{"name":"Mock","base_url":base_url,"model":"mock-model"}]});
    std::fs::write(settings.join("ai-providers.json"), preferences.to_string()).unwrap();
    format!("{base_url}/chat/completions")
}

#[cfg(target_os = "macos")]
#[test]
fn ai_translate_sends_the_target_language_and_only_confirmed_pages() {
    ai_provider_mock(true, "ai_translate");
}

#[cfg(target_os = "macos")]
#[test]
fn ai_llama_cpp_discovers_models_and_runs_all_document_actions() {
    use std::net::TcpListener;

    use serde_json::{Value, json};

    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let pdf = root.join("fixture.pdf");
    std::fs::write(&pdf, fixture(2)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let endpoint = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
    let question = "What is on page 2?";
    let page_text = "[Page 2]\nPage 2\n\n";
    let profile = json!({"name":"llama.cpp", "base_url":base_url, "model":"local-model", "llama_cpp":true});
    let settings = root.join("Library/Application Support/PdfCraft");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(settings.join("ai-providers.json"), json!({"active":"llama.cpp", "profiles":[profile]}).to_string()).unwrap();

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            for _ in 0..2 {
                let (socket, body) = ai_mock_request(&listener, "GET /v1/models");
                assert!(body.is_empty());
                ai_mock_reply(socket, r#"{"data":[{"id":"local-model"},{"id":"other-model"},{"id":"local-model"},{"id":""}]}"#);
            }
            let requests = [
                (json!(page_text), "Local summary."),
                (json!({"question":question,"recent_turns":[],"sources":[{"source":1,"page":2,"text":"Page 2"}]}), "The text is Page 2. [S1]"),
                (json!({"target_language":"Spanish","text":page_text}), "Página 2"),
            ];
            for (expected, answer) in requests {
                let (socket, body) = ai_mock_request(&listener, "POST /v1/chat/completions");
                let payload: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(payload["model"], "local-model");
                assert_eq!(payload["stream"], false);
                let input = payload["messages"][1]["content"].as_str().unwrap();
                if expected.is_object() {
                    assert_eq!(serde_json::from_str::<Value>(input).unwrap(), expected);
                } else {
                    assert_eq!(input, expected.as_str().unwrap());
                }
                ai_mock_reply(socket, &json!({"choices":[{"message":{"content":answer},"finish_reason":"stop"}]}).to_string());
            }
        });
        let (ok, report) = run_ai_script(
            root,
            json!([
                {"tool":"ai_llama_models","args":{"base_url":base_url}},
                {"tool":"ai_llama_models","args":{"provider":"llama.cpp"}},
                {"tool":"doc_open","args":{"path":pdf}},
                {"tool":"ai_summarize","args":{"doc":1,"first_page":2,"last_page":2}},
                {"tool":"ai_summarize","args":{"doc":1,"first_page":2,"last_page":2,"user_confirmed":true,
                    "confirmed_endpoint":endpoint,"confirmed_model":"local-model"}},
                {"tool":"ai_ask","args":{"doc":1,"first_page":2,"last_page":2,"question":question,"user_confirmed":true,
                    "confirmed_endpoint":endpoint,"confirmed_model":"local-model"}},
                {"tool":"ai_translate","args":{"doc":1,"first_page":2,"last_page":2,"target_language":"Spanish","user_confirmed":true,
                    "confirmed_endpoint":endpoint,"confirmed_model":"local-model"}}
            ]),
        );
        server.join().unwrap();
        assert!(ok, "{report}");
        let result = |index: usize| &report["steps"][index]["result"][0];
        assert_eq!(result(0)["models"], json!(["local-model", "other-model"]));
        assert_eq!(result(1), result(0));
        assert_eq!(result(3)["endpoint"], endpoint);
        assert_eq!(result(3)["sent"], false);
        for index in 4..=6 {
            assert_eq!(result(index)["sent"], true);
        }
        assert_eq!(result(4)["summary"], "Local summary.");
        assert_eq!(result(5)["answer"], "The text is Page 2. [Page 2]");
        assert_eq!(result(5)["citations"], json!([{"page":2,"excerpt":"Page 2"}]));
        assert_eq!(result(6)["translation"], "Página 2");
    });
}

#[cfg(target_os = "macos")]
fn run_ai_script(root: &std::path::Path, steps: serde_json::Value) -> (bool, serde_json::Value) {
    let script = root.join("ai-script.json");
    std::fs::write(&script, steps.to_string()).unwrap();
    let output = Command::new(BIN).env("HOME", root).args(["run", "--report", "--script"]).arg(&script).arg("--root").arg(root).output().unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
    (output.status.success(), report)
}

#[cfg(target_os = "macos")]
pub(super) fn ai_mock_request(listener: &std::net::TcpListener, request: &str) -> (std::net::TcpStream, Vec<u8>) {
    use std::io::{BufRead, BufReader, ErrorKind, Read};
    use std::time::{Duration, Instant};

    // Keep the mock bounded even if a regression prevents the request.
    listener.set_nonblocking(true).unwrap();
    let start = Instant::now();
    let mut socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(e) if e.kind() == ErrorKind::WouldBlock && start.elapsed() < Duration::from_secs(5) => {
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
    assert_eq!(line, format!("{request} HTTP/1.1\r\n"));
    let mut length = 0;
    loop {
        line.clear();
        assert_ne!(read.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            assert!(!name.eq_ignore_ascii_case("Authorization"), "keyless local profile");
            if name.eq_ignore_ascii_case("Content-Length") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
    }
    let mut body = vec![0; length];
    read.read_exact(&mut body).unwrap();
    (socket, body)
}

#[cfg(target_os = "macos")]
pub(super) fn ai_mock_reply(mut socket: std::net::TcpStream, reply: &str) {
    use std::io::Write;

    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len())
        .unwrap();
}

#[cfg(target_os = "macos")]
fn ai_provider_mock(responses: bool, tool: &str) {
    let ask = tool == "ai_ask";
    let translate = tool == "ai_translate";
    use std::net::TcpListener;

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
        let mut steps = vec![json!({"tool":"doc_open","args":{"path":pdf}})];
        if mark {
            steps.push(json!({"tool":"redact_mark","args":{"doc":1,"page":2,"whole_pages":true}}));
        }
        steps.push(json!({"tool":tool,"args":args}));
        run_ai_script(root.path(), json!(steps))
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
            let (socket, body) = ai_mock_request(&listener, &format!("POST {route}"));
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
            ai_mock_reply(socket, reply);
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
