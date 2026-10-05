//! Host-side test for 46-UAT gap 2 (test 5, major): the /alerts sort control ("I don't see a
//! 'Most recent' field") and the same weak-affordance pattern shared by every other /alerts
//! control button. No DOM, no headless browser (phase 42 research Pitfall 10) — this proves the
//! contrast math and the presence of the CSS rules directly from `tokens.css`'s source text, and
//! (in the second half of this file, added by Task 2) scans the component source for the button
//! markup that must carry the new class.
//!
//! WCAG 1.4.11 (Non-text Contrast) requires a UI component boundary to be at least 3:1 against
//! its background. `--border` (#1e293b on #0b0f19) is ~1.3:1 — that is the root cause this plan
//! fixes, not a hypothetical.

const TOKENS_CSS: &str = include_str!("../style/tokens.css");

/// Strips `/* ... */` block comments from `tokens.css` before any substring assertion on a rule
/// body — this file is heavily commented, and a comment can satisfy a grep-style check on its own
/// (the plan-grep self-trip lesson recorded for this project: a comment mentioning the property
/// name must not count as the property being set).
fn strip_block_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next(); // consume '*'
            let mut prev = ' ';
            for next in chars.by_ref() {
                if prev == '*' && next == '/' {
                    break;
                }
                prev = next;
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Slices the source between `selector { ... }`'s opening and matching closing brace. Panics
/// (rather than returning an empty/default slice) if the selector is not found, so a missing
/// block fails loudly instead of the property lookup below silently returning nothing.
fn block_body<'a>(src: &'a str, selector: &str) -> &'a str {
    let start = src
        .find(&format!("{selector} {{"))
        .unwrap_or_else(|| panic!("selector `{selector}` not found in tokens.css"));
    let body_start = start + selector.len() + 2; // past "{selector} {"
    let mut depth = 1usize;
    let bytes = src.as_bytes();
    let mut i = body_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[body_start..i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("selector `{selector}` block in tokens.css has no matching closing brace");
}

/// Pulls a `--name: #rrggbb;` custom property's hex value out of a block body. Panics with the
/// property name if it is missing, rather than defaulting — a missing property must fail the
/// test, not silently pass with a placeholder colour (non-vacuity).
fn custom_property_hex(block: &str, name: &str) -> (u8, u8, u8) {
    let needle = format!("--{name}:");
    let idx = block
        .find(&needle)
        .unwrap_or_else(|| panic!("custom property --{name} not found in this block"));
    let rest = &block[idx + needle.len()..];
    let hash = rest
        .find('#')
        .unwrap_or_else(|| panic!("--{name} has no #rrggbb value"));
    let hex = &rest[hash + 1..hash + 7];
    let r = u8::from_str_radix(&hex[0..2], 16).expect("valid hex red channel");
    let g = u8::from_str_radix(&hex[2..4], 16).expect("valid hex green channel");
    let b = u8::from_str_radix(&hex[4..6], 16).expect("valid hex blue channel");
    (r, g, b)
}

/// WCAG relative luminance: sRGB channel to linear, c <= 0.03928 -> c/12.92, else
/// ((c+0.055)/1.055)^2.4; L = 0.2126R + 0.7152G + 0.0722B.
fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let chan = |c: u8| -> f64 {
        let c = c as f64 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * chan(r) + 0.7152 * chan(g) + 0.0722 * chan(b)
}

/// (Lhi+0.05)/(Llo+0.05), always with the lighter colour on top regardless of argument order.
fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn root_block(src: &str) -> &str {
    block_body(src, ":root")
}

fn light_block(src: &str) -> &str {
    block_body(src, ".light")
}

#[test]
fn control_button_boundary_is_at_least_3_to_1_against_background_and_card_both_themes() {
    let clean = strip_block_comments(TOKENS_CSS);

    for (theme, block) in [("dark", root_block(&clean)), ("light", light_block(&clean))] {
        let muted_foreground = custom_property_hex(block, "muted-foreground");
        let background = custom_property_hex(block, "background");
        let card = custom_property_hex(block, "card");

        let vs_background = contrast_ratio(muted_foreground, background);
        assert!(
            vs_background >= 3.0,
            "{theme}: --muted-foreground vs --background contrast is {vs_background:.2}, need >= 3.0"
        );

        let vs_card = contrast_ratio(muted_foreground, card);
        assert!(
            vs_card >= 3.0,
            "{theme}: --muted-foreground vs --card contrast is {vs_card:.2}, need >= 3.0"
        );
    }
}

#[test]
fn pressed_segment_text_on_fill_is_at_least_4_5_to_1_both_themes() {
    let clean = strip_block_comments(TOKENS_CSS);

    for (theme, block) in [("dark", root_block(&clean)), ("light", light_block(&clean))] {
        let foreground = custom_property_hex(block, "foreground");
        let background = custom_property_hex(block, "background");
        let ratio = contrast_ratio(foreground, background);
        assert!(
            ratio >= 4.5,
            "{theme}: pressed-segment fill --foreground vs text --background contrast is \
             {ratio:.2}, need >= 4.5"
        );
    }
}

#[test]
fn tokens_css_defines_the_pressed_segment_rule() {
    let clean = strip_block_comments(TOKENS_CSS);
    let selector = ".control-button[aria-pressed=\"true\"]";
    assert!(
        clean.contains(selector),
        "tokens.css must contain a rule whose selector includes `{selector}`"
    );
    let body = block_body(&clean, selector);
    // Substrings tied together deliberately (not independent `contains` checks) — a rule that
    // sets `background: var(--primary)` alongside an unrelated `border-color: var(--foreground)`
    // must NOT satisfy this, since it is the wrong fill colour.
    assert!(
        body.contains("background: var(--foreground)"),
        "the pressed-segment rule must set background to var(--foreground): {body}"
    );
    assert!(
        body.contains("color: var(--background)"),
        "the pressed-segment rule must set color to var(--background): {body}"
    );
}

#[test]
fn tokens_css_defines_segmented_and_control_button_with_hit_target_and_boundary() {
    let clean = strip_block_comments(TOKENS_CSS);
    assert!(
        clean.contains(".segmented"),
        "tokens.css must define a .segmented selector"
    );
    let body = block_body(&clean, ".control-button");
    assert!(
        body.contains("min-height: 44px"),
        ".control-button must declare min-height: 44px: {body}"
    );
    assert!(
        body.contains("min-width: 44px"),
        ".control-button must declare min-width: 44px: {body}"
    );
    assert!(
        body.contains("border: 1px solid var(--muted-foreground)"),
        ".control-button must declare border: 1px solid var(--muted-foreground): {body}"
    );
}

// --- Task 2: /alerts markup scan -------------------------------------------------------------

const ALERTS_MOD_RS: &str = include_str!("../src/alerts/mod.rs");
const ALERTS_FILTERS_RS: &str = include_str!("../src/alerts/filters.rs");
const ALERTS_ROW_RS: &str = include_str!("../src/alerts/row.rs");
const ALERTS_WITHDRAW_RS: &str = include_str!("../src/alerts/withdraw.rs");
const INVESTIGATION_PIN_RS: &str = include_str!("../src/investigation/pin.rs");

/// Strips `//` line comments before scanning source for button markup.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn sort_buttons_share_one_segmented_group_and_both_carry_control_button() {
    let clean = strip_line_comments(ALERTS_MOD_RS);
    let segmented_idx = clean
        .find("class=\"segmented\"")
        .expect("alerts/mod.rs must have an element with class=\"segmented\"");
    let sort_score_idx = clean
        .find("{SORT_SCORE}")
        .expect("SORT_SCORE must be rendered");
    let sort_recency_idx = clean
        .find("{SORT_RECENCY}")
        .expect("SORT_RECENCY must be rendered");
    assert!(
        segmented_idx < sort_score_idx && sort_score_idx < sort_recency_idx,
        "both sort buttons must sit inside (after) the .segmented wrapper"
    );

    // Both sort buttons' preceding <button ...> opening tag must carry control-button.
    for label in ["{SORT_SCORE}", "{SORT_RECENCY}"] {
        let label_idx = clean.find(label).unwrap();
        let button_start = clean[..label_idx]
            .rfind("<button")
            .expect("a <button opening tag must precede the sort label");
        let button_end = button_start
            + clean[button_start..]
                .find('>')
                .expect("the <button tag must close with >");
        let tag = &clean[button_start..=button_end];
        assert!(
            tag.contains("control-button"),
            "the button rendering {label} must carry the control-button class: {tag}"
        );
    }
}

/// Scans every `<button ... >` opening tag (from `<button` to its closing `>`) in the given
/// source, returning each tag's full text. Scoped to button elements specifically — the rail's
/// `<section>`, the row card and `<input>`/`<textarea>` elements legitimately keep `--border`.
fn button_tags(src: &str) -> Vec<&str> {
    let mut tags = Vec::new();
    let mut rest = src;
    let mut consumed = 0usize;
    while let Some(start) = rest.find("<button") {
        let abs_start = consumed + start;
        let after = &src[abs_start..];
        let end = after
            .find('>')
            .unwrap_or_else(|| panic!("a <button tag has no closing > near byte {abs_start}"));
        tags.push(&src[abs_start..=abs_start + end]);
        consumed = abs_start + end + 1;
        rest = &src[consumed..];
    }
    tags
}

#[test]
fn no_alerts_button_keeps_the_faint_inline_border() {
    let sources = [
        ("alerts/mod.rs", ALERTS_MOD_RS),
        ("alerts/filters.rs", ALERTS_FILTERS_RS),
        ("alerts/row.rs", ALERTS_ROW_RS),
        ("alerts/withdraw.rs", ALERTS_WITHDRAW_RS),
        ("investigation/pin.rs", INVESTIGATION_PIN_RS),
    ];

    let mut total_inspected = 0usize;
    for (name, src) in sources {
        let clean = strip_line_comments(src);
        let tags = button_tags(&clean);
        for tag in &tags {
            assert!(
                !tag.contains("border: 1px solid var(--border)"),
                "{name}: a <button> still carries the faint inline border: {tag}"
            );
        }
        total_inspected += tags.len();
    }

    assert!(
        total_inspected >= 17,
        "expected to inspect at least 17 <button> elements across the five /alerts sources, \
         found {total_inspected} — this test must not pass by finding none"
    );
}

#[test]
fn every_named_control_button_carries_the_control_button_class() {
    let mod_clean = strip_line_comments(ALERTS_MOD_RS);
    let filters_clean = strip_line_comments(ALERTS_FILTERS_RS);
    let row_clean = strip_line_comments(ALERTS_ROW_RS);
    let withdraw_clean = strip_line_comments(ALERTS_WITHDRAW_RS);
    let pin_clean = strip_line_comments(INVESTIGATION_PIN_RS);

    // Helper: for EVERY occurrence of a marker string (not just the first — `str::find` alone
    // would leave later call sites of a repeated marker unchecked, 46-REVIEW-GAPS WR-01), find
    // the nearest preceding <button ...> tag and assert it carries control-button. The
    // occurrence count must equal `expected`, so a marker that vanishes (or gains a new,
    // unchecked call site) fails the test instead of passing vacuously.
    let assert_button_before = |src: &str, marker: &str, file: &str, expected: usize| {
        let mut seen = 0usize;
        for (marker_idx, _) in src.match_indices(marker) {
            seen += 1;
            let button_start = src[..marker_idx].rfind("<button").unwrap_or_else(|| {
                panic!("{file}: no <button before occurrence {seen} of marker `{marker}`")
            });
            let button_end = button_start
                + src[button_start..].find('>').unwrap_or_else(|| {
                    panic!("{file}: <button before occurrence {seen} of `{marker}` never closes")
                });
            let tag = &src[button_start..=button_end];
            assert!(
                tag.contains("control-button"),
                "{file}: button before occurrence {seen} of `{marker}` must carry \
                 control-button: {tag}"
            );
        }
        assert_eq!(
            seen, expected,
            "{file}: expected marker `{marker}` to occur exactly {expected} time(s), found {seen}"
        );
    };

    // alerts/mod.rs — `{SHOW_THEM}` renders twice: the "all results withdrawn" empty state and
    // the withdrawn-toggle row beside `{HIDE_THEM}`. Both buttons are checked.
    assert_button_before(&mod_clean, "{RETRY}", "alerts/mod.rs", 1);
    assert_button_before(&mod_clean, "{SHOW_THEM}", "alerts/mod.rs", 2);
    assert_button_before(&mod_clean, "{HIDE_THEM}", "alerts/mod.rs", 1);
    assert_button_before(&mod_clean, "{CLEAR_FILTERS}", "alerts/mod.rs", 1);
    assert_button_before(&mod_clean, "{SHOW_MORE}", "alerts/mod.rs", 1);

    // alerts/filters.rs
    assert_button_before(&filters_clean, "\"Clear filters\"", "alerts/filters.rs", 1);
    assert_button_before(&filters_clean, "{*label}", "alerts/filters.rs", 1);

    // alerts/row.rs
    assert_button_before(&row_clean, "\"Withdraw…\"", "alerts/row.rs", 1);

    // alerts/withdraw.rs
    assert_button_before(
        &withdraw_clean,
        "{WITHDRAW_CANCEL}",
        "alerts/withdraw.rs",
        1,
    );

    // investigation/pin.rs
    assert_button_before(
        &pin_clean,
        "id=\"pin-to-investigation\"",
        "investigation/pin.rs",
        1,
    );
    assert_button_before(&pin_clean, "{name}", "investigation/pin.rs", 1);
}
