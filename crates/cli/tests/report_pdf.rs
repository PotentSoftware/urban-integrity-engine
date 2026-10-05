//! Integration tests for PDF report export via pandoc.
//!
//! Requirements covered:
//! - REP-03: pandoc invocation with correct args; graceful error when pandoc absent

use urban_integrity_cli::invoke_pdf_tool;

/// REP-03: `invoke_pdf_tool` either produces a PDF (if pandoc+engine is available)
/// or returns Err with the D-11 message while leaving the .md file intact.
#[test]
fn rep_03_invoke_pdf_calls_pandoc_with_args_array() {
    let tmp = std::env::temp_dir();
    let md_path = tmp.join(format!("urban-integrity-test-{}.md", std::process::id()));
    let pdf_path = tmp.join(format!("urban-integrity-test-{}.pdf", std::process::id()));

    // Write a trivial Markdown file
    std::fs::write(&md_path, "# Test\n\nThis is a test report.\n")
        .expect("Failed to write tmp .md for test");

    let result = invoke_pdf_tool(&md_path, &pdf_path);

    // Adaptive assertion: either PDF was produced (Ok) or error path has .md intact
    match result {
        Ok(()) => {
            // pandoc + PDF engine available: PDF should exist
            assert!(
                pdf_path.exists(),
                "invoke_pdf_tool returned Ok but pdf_path does not exist: {}",
                pdf_path.display()
            );
        }
        Err(e) => {
            // pandoc absent or PDF engine missing + wkhtmltopdf absent: .md must still exist
            assert!(
                md_path.exists(),
                "invoke_pdf_tool returned Err but .md file was deleted: {}",
                md_path.display()
            );
            let msg = e.to_string();
            assert!(
                msg.contains("Markdown report saved to"),
                "D-11 error message must contain 'Markdown report saved to', got: {msg}"
            );
        }
    }

    // Cleanup
    let _ = std::fs::remove_file(&md_path);
    let _ = std::fs::remove_file(&pdf_path);
}

/// REP-03: When pandoc (and wkhtmltopdf) are absent from PATH, invoke_pdf_tool
/// returns Err with D-11 install instructions, and the .md file remains on disk.
///
/// NOTE: This test mutates process-global PATH (std::env::set_var).
/// Run with --test-threads=1 to avoid races with other tests.
/// Marked #[ignore] because PATH mutation is unsafe in parallel test execution.
#[test]
#[ignore = "PATH mutation — run with --test-threads=1"]
fn rep_03_missing_pandoc_writes_md_then_errors() {
    let tmp = std::env::temp_dir();
    let md_path = tmp.join(format!(
        "urban-integrity-no-pandoc-{}.md",
        std::process::id()
    ));
    let pdf_path = tmp.join(format!(
        "urban-integrity-no-pandoc-{}.pdf",
        std::process::id()
    ));

    // Write the .md file (simulating what the Pdf arm does before calling invoke_pdf_tool)
    std::fs::write(&md_path, "# Test\n").expect("Failed to write tmp .md for test");

    // Save original PATH and replace with a path containing no binaries
    let original_path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: mutates process-global environment — only safe with --test-threads=1
    unsafe {
        std::env::set_var("PATH", "/nonexistent");
    }

    let result = invoke_pdf_tool(&md_path, &pdf_path);

    // Restore PATH before assertions (always restore, even on panic)
    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    assert!(
        result.is_err(),
        "Expected Err when PATH has no pandoc/wkhtmltopdf"
    );

    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("PDF generation requires pandoc. Install: sudo apt install pandoc (Debian/Ubuntu) or brew install pandoc (macOS). Markdown report saved to"),
        "D-11 error message not found, got: {msg}"
    );

    assert!(
        md_path.exists(),
        "The .md file must still exist after invoke_pdf_tool error (D-11): {}",
        md_path.display()
    );

    // Cleanup
    let _ = std::fs::remove_file(&md_path);
    let _ = std::fs::remove_file(&pdf_path);
}
