//! Output budgeting: every tool result must fit the host's inline cap.

/// Hard inline cap in UTF-8 bytes; above this hosts spill output to disk.
pub const CAP: usize = 8800;

const NOTICE_RESERVE: usize = 64;

/// Caller-supplied `maxChars` clamped into `[lo, CAP]`.
pub fn limit(max_chars: Option<usize>, lo: usize) -> usize {
    max_chars.unwrap_or(CAP).clamp(lo.min(CAP), CAP)
}

/// Head-tail fold: keep the start and the end, replace the middle with a notice.
/// The result never exceeds `max` bytes and never splits a UTF-8 character.
pub fn fold(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    if max <= NOTICE_RESERVE * 2 {
        return s[..s.floor_char_boundary(max)].to_string();
    }
    let keep = max - NOTICE_RESERVE;
    let head = s.floor_char_boundary(keep * 2 / 3);
    let tail = s.ceil_char_boundary(s.len() - keep / 3);
    format!(
        "{}\n... [truncated: {} bytes omitted] ...\n{}",
        &s[..head],
        tail - head,
        &s[tail..]
    )
}

/// Fair batch budgeting: split `max` bytes across `parts`. Small parts keep
/// their full text and their unused share is redistributed to the larger ones,
/// which are head-tail folded. Order is preserved.
pub fn fair(parts: &[String], max: usize) -> Vec<String> {
    let lens: Vec<usize> = parts.iter().map(String::len).collect();
    fair_by(&lens, max, |i, share| fold(&parts[i], share))
}

/// `fair` for parts known by their full length `lens[i]`; `fit(i, share)` renders
/// part `i` in at most `share` bytes.
pub fn fair_by(lens: &[usize], max: usize, mut fit: impl FnMut(usize, usize) -> String) -> Vec<String> {
    let mut order: Vec<usize> = (0..lens.len()).collect();
    order.sort_by_key(|&i| lens[i]);
    let mut out = vec![String::new(); lens.len()];
    let mut remaining = max;
    for (done, &i) in order.iter().enumerate() {
        let share = remaining / (lens.len() - done);
        out[i] = fit(i, share);
        remaining -= out[i].len().min(remaining);
    }
    out
}

/// Join parts with a blank line, giving each a fair share of `max`.
pub fn join_fair(parts: &[String], max: usize) -> String {
    let lens: Vec<usize> = parts.iter().map(String::len).collect();
    join_fair_by(&lens, max, |i, share| fold(&parts[i], share))
}

/// `join_fair` over `fair_by` parts.
pub fn join_fair_by(lens: &[usize], max: usize, fit: impl FnMut(usize, usize) -> String) -> String {
    let seps = 2 * lens.len().saturating_sub(1);
    if seps >= max {
        // The separators alone would fill the budget: say so instead of emitting blank lines.
        return fold(&format!("[{} sections omitted: they do not fit in {max} bytes; request fewer items]", lens.len()), max);
    }
    fair_by(lens, max - seps, fit).join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_is_utf8_safe_and_bounded() {
        let s = "привіт 🌍 ".repeat(2000);
        for max in [1, 7, 100, 129, 500, 8800] {
            let f = fold(&s, max);
            assert!(f.len() <= max, "max {max} got {}", f.len());
        }
        let f = fold(&s, 500);
        assert!(f.contains("[truncated:"));
        assert!(f.starts_with("привіт") && f.ends_with("🌍 "));
        assert_eq!(fold("short", 100), "short");
    }

    #[test]
    fn fair_redistributes_unused_share() {
        let parts = vec!["a".repeat(10), "b".repeat(5000), "c".repeat(5000)];
        let out = fair(&parts, 1000);
        assert_eq!(out[0], parts[0]);
        assert!(out[1].len() > 400 && out[2].len() > 400);
        assert!(out.iter().map(String::len).sum::<usize>() <= 1000);
        assert!(join_fair(&parts, 1000).len() <= 1000);
    }

    #[test]
    fn join_never_exceeds_max_when_separators_fill_it() {
        let parts = vec!["x".to_string(); 550];
        let out = join_fair(&parts, 1000);
        assert!(out.len() <= 1000 && out.starts_with("[550 sections omitted"), "{out}");
        assert_eq!(join_fair(&parts[..3], 1000), "x\n\nx\n\nx");
    }
}
