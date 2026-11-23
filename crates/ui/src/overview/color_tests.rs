use super::*;

/// Pins `hash_color` against `session_canvas.html`'s `hashColor`
/// (`~889-893`) computed independently in Python with the same
/// `(hash * 31 + charCode) & 0xFFFFFFFF` truncation JS's `>>> 0`
/// performs, to catch a signed/unsigned overflow divergence rather than
/// just eyeballing the Rust output.
#[test]
fn hash_color_matches_reference_js_algorithm() {
    let cases: &[(&str, u32)] = &[
        ("zeron", 6),
        ("ui", 4),
        ("ENG-1234", 1),
        ("agent-mode-tools", 1),
        ("a", 1),
    ];
    for (key, expected_idx) in cases {
        let expected: gpui::Hsla = gpui::rgb(HASH_COLOR_PALETTE[*expected_idx as usize]).into();
        assert_eq!(
            hash_color(key),
            expected,
            "hash_color({key:?}) should land on palette index {expected_idx}"
        );
    }
}

#[test]
fn hash_color_empty_key_is_index_zero() {
    let expected: gpui::Hsla = gpui::rgb(HASH_COLOR_PALETTE[0]).into();
    assert_eq!(hash_color(""), expected);
}

#[test]
fn none_key_is_always_text_muted_regardless_of_dimension() {
    let expected: gpui::Hsla = gpui::rgb(0x898781).into();
    for dim in [GroupDimension::Repo, GroupDimension::Ticket] {
        assert_eq!(group_color(dim, overview_grouping::NONE_KEY), expected);
    }
}
