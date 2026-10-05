//! `Timestamp` — the one shared relative+absolute time component used everywhere a point in time
//! renders (Home cards, BLUF box, accordion probe detail, Graph page), per the phase 43 UI spec's
//! "Timestamp component" section (D-02, NAV-03). Never render a raw RFC 3339 string directly.
//!
//! # Testing approach (plain-function-first, per `verdict_badge.rs`'s documented rationale)
//!
//! The plan's interface note describes `relative_time`/`absolute_time` taking `js_sys::Date`
//! parameters directly. That signature turns out to be untestable on this crate's host test
//! target: `js_sys::Date`'s accessors panic with "cannot call wasm-bindgen imported functions on
//! non-wasm targets" the moment a `js_sys::Date` value is touched outside an actual wasm/JS
//! runtime (confirmed by a throwaway spike before writing this file). Since every other
//! formatting function in this crate is host-tested with no DOM (`verdict_badge.rs`'s module
//! doc), this file instead extracts a DOM-free [`DateParts`] snapshot from a `js_sys::Date` in
//! exactly one place ([`DateParts::from_date`]) and passes plain `f64`/`DateParts` values into
//! [`relative_time`]/[`absolute_time`]/[`date_only`], which are fully host-testable. Only the
//! `#[component] Timestamp` wrapper itself touches `js_sys::Date`, matching the crate's
//! plain-function/component split (Rule 1 deviation — see this plan's Summary).
//!
//! Module-wide `#![allow(dead_code)]`: this is an interface-first plan (Wave 3 page plans 06-08
//! are `Timestamp`'s first real callers) — nothing in `App`'s tree renders it yet, so the
//! `#[component]` macro's generated `TimestampProps` struct field (`at`) reads as genuinely
//! unused to a bin-target build. A per-item `#[allow(dead_code)]` on the `Timestamp` fn itself
//! does not reach that macro-generated struct (leptos_macro's `Prop` only forwards `doc`/`prop`
//! attributes onto struct fields, confirmed by reading `leptos_macro-0.8.17/src/component.rs`),
//! so the module-level form is the only mechanism that also covers it.
#![allow(dead_code)]

use leptos::prelude::*;

/// A DOM/JS-free snapshot of a `js_sys::Date`'s local-time fields — the seam that keeps every
/// formatting function below host-testable (see module doc).
// Real consumer is the `Timestamp` component below; nothing in `App`'s tree renders `Timestamp`
// yet (Wave 3 page plans 06-08 are its first callers), so this whole module reads as dead code
// to a bin-target build until then — matching `glossary.rs`'s established `#[allow(dead_code)]`
// convention for interface-first plans in this phase.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateParts {
    pub day: u32,
    /// 0-indexed, matching `js_sys::Date::get_month()`'s JS convention (0 = January).
    pub month0: u32,
    pub year: i32,
    pub hour: u32,
    pub minute: u32,
}

impl DateParts {
    /// Extracts local-time date parts from a `js_sys::Date`. Deliberately never reads
    /// `get_milliseconds()` — no sub-second digit ever reaches [`absolute_time`]/[`date_only`].
    #[allow(dead_code)]
    pub fn from_date(date: &js_sys::Date) -> Self {
        DateParts {
            day: date.get_date(),
            month0: date.get_month(),
            year: date.get_full_year() as i32,
            hour: date.get_hours(),
            minute: date.get_minutes(),
        }
    }
}

const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

#[allow(dead_code)]
fn month_name(month0: u32) -> &'static str {
    MONTH_NAMES.get(month0 as usize).copied().unwrap_or("Jan")
}

/// The absolute date alone, e.g. `"12 Aug 2026"` — used both as `relative_time`'s >30-day
/// fallback and as a building block for [`absolute_time`].
#[allow(dead_code)]
pub fn date_only(parts: &DateParts) -> String {
    format!("{} {} {}", parts.day, month_name(parts.month0), parts.year)
}

/// `"{day} {month} {year}, {hour}:{minute}"`, 24-hour clock, always zero-padded minutes, never a
/// seconds or millisecond fragment (the phase 43 UI spec "Timestamp component": "No sub-second digits
/// ever, in either fragment").
#[allow(dead_code)]
pub fn absolute_time(parts: &DateParts) -> String {
    format!(
        "{}, {:02}:{:02}",
        date_only(parts),
        parts.hour,
        parts.minute
    )
}

/// The relative-time fragment: "just now" (<60s), "{N} minutes ago" (<60min), "{N} hours ago"
/// (<24h), "{N} days ago" (<30 days), else the absolute date alone (the phase 43 UI spec's threshold
/// ladder, discretion per the design notes). `diff_ms` is clamped to non-negative so a clock-skew edge
/// case never renders a negative or nonsensical string.
#[allow(dead_code)]
pub fn relative_time(now_ms: f64, then_ms: f64, then_parts: &DateParts) -> String {
    let diff_secs = (now_ms - then_ms).max(0.0) / 1000.0;
    if diff_secs < 60.0 {
        "just now".to_string()
    } else if diff_secs < 3600.0 {
        let minutes = (diff_secs / 60.0).floor() as i64;
        format!("{minutes} minutes ago")
    } else if diff_secs < 86_400.0 {
        let hours = (diff_secs / 3600.0).floor() as i64;
        format!("{hours} hours ago")
    } else if diff_secs < 86_400.0 * 30.0 {
        let days = (diff_secs / 86_400.0).floor() as i64;
        format!("{days} days ago")
    } else {
        date_only(then_parts)
    }
}

/// Renders `{relative} · {absolute}` (absolute in `.font-mono`/`--muted-foreground`), with the
/// absolute string also set as the element's `title` attribute (D-02's redundant-tooltip rule).
/// `at` is an RFC 3339 string, parsed via `js_sys::Date::new`.
#[component]
#[allow(non_snake_case, dead_code)]
pub fn Timestamp(at: String) -> impl IntoView {
    let then = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(&at));
    let then_parts = DateParts::from_date(&then);
    let then_ms = then.get_time();
    let now_ms = js_sys::Date::now();
    let relative = relative_time(now_ms, then_ms, &then_parts);
    let absolute = absolute_time(&then_parts);
    let title_absolute = absolute.clone();

    view! {
        <span class="text-body" title=title_absolute>
            {relative}
            <span aria-hidden="true">" · "</span>
            <span class="font-mono" style="color: var(--muted-foreground);">{absolute}</span>
        </span>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(day: u32, month0: u32, year: i32, hour: u32, minute: u32) -> DateParts {
        DateParts {
            day,
            month0,
            year,
            hour,
            minute,
        }
    }

    #[test]
    fn thirty_seconds_before_now_is_just_now() {
        let now_ms = 1_700_000_000_000.0;
        let then_ms = now_ms - 30_000.0;
        assert_eq!(
            relative_time(now_ms, then_ms, &parts(1, 0, 2026, 0, 0)),
            "just now"
        );
    }

    #[test]
    fn five_minutes_before_now() {
        let now_ms = 1_700_000_000_000.0;
        let then_ms = now_ms - 5.0 * 60_000.0;
        assert_eq!(
            relative_time(now_ms, then_ms, &parts(1, 0, 2026, 0, 0)),
            "5 minutes ago"
        );
    }

    #[test]
    fn three_hours_before_now() {
        let now_ms = 1_700_000_000_000.0;
        let then_ms = now_ms - 3.0 * 3_600_000.0;
        assert_eq!(
            relative_time(now_ms, then_ms, &parts(1, 0, 2026, 0, 0)),
            "3 hours ago"
        );
    }

    #[test]
    fn ten_days_before_now() {
        let now_ms = 1_700_000_000_000.0;
        let then_ms = now_ms - 10.0 * 86_400_000.0;
        assert_eq!(
            relative_time(now_ms, then_ms, &parts(1, 0, 2026, 0, 0)),
            "10 days ago"
        );
    }

    /// PROVE THIS CAN FAIL: without the `else` fallback branch, 40 days would fall through the
    /// `<30 days` arm's `.floor()` math into a nonsensical or negative-looking string. This test
    /// asserts the fallback is the bare absolute date, never a relative phrase.
    #[test]
    fn forty_days_before_now_falls_back_to_absolute_date_alone() {
        let now_ms = 1_700_000_000_000.0;
        let then_ms = now_ms - 40.0 * 86_400_000.0;
        let then_parts = parts(12, 7, 2026, 0, 0); // 12 Aug 2026 (month0=7 -> August)
        let result = relative_time(now_ms, then_ms, &then_parts);
        assert_eq!(result, "12 Aug 2026");
        assert!(!result.contains("ago"));
        assert!(!result.starts_with('-'));
    }

    #[test]
    fn absolute_time_never_contains_subsecond_digits() {
        let s = absolute_time(&parts(4, 8, 2026, 20, 27));
        assert_eq!(s, "4 Sep 2026, 20:27");
        assert!(
            !s.contains('.'),
            "absolute_time must not contain a decimal point: {s}"
        );
        assert_eq!(
            s.matches(':').count(),
            1,
            "absolute_time must carry only hour:minute, no seconds fragment: {s}"
        );
    }
}
