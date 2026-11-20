use super::*;

pub(super) fn tool_row_reveal_progress(start: Option<Instant>, now: Instant, reduce_motion: bool) -> f32 {
    let Some(start) = start.filter(|_| !reduce_motion) else {
        return 1.0;
    };
    let elapsed = now.checked_duration_since(start).unwrap_or_default();
    let raw = elapsed.as_secs_f32() / TOOL_ROW_REVEAL.total().as_secs_f32();
    TOOL_ROW_REVEAL.curve.eval(raw)
}

pub(super) fn tool_connector_reveal_progress(
    start: Option<Instant>,
    now: Instant,
    reduce_motion: bool,
) -> f32 {
    let Some(start) = start.filter(|_| !reduce_motion) else {
        return 1.0;
    };
    let elapsed = now.checked_duration_since(start).unwrap_or_default();
    let raw = elapsed.as_secs_f32() / TOOL_CONNECTOR_REVEAL.total().as_secs_f32();
    TOOL_CONNECTOR_REVEAL.curve.eval(raw)
}

/// Split one arrival into a continuous tree draw. For child rows, the previous
/// row grows its continuation to the boundary first, then this row draws its
/// incoming trunk and elbow. The branch slightly overlaps the end of the
/// incoming phase so there is no dead frame at the bend.
pub(super) fn tool_connector_parts(progress: f32, has_predecessor: bool) -> (f32, f32) {
    let progress = progress.clamp(0.0, 1.0);
    let (incoming_start, incoming_end, branch_start) = if has_predecessor {
        (0.45, 0.72, 0.68)
    } else {
        (0.0, 0.62, 0.58)
    };
    let incoming = ((progress - incoming_start) / (incoming_end - incoming_start)).clamp(0.0, 1.0);
    let branch = ((progress - branch_start) / (1.0 - branch_start)).clamp(0.0, 1.0);
    (incoming, branch)
}

/// The outgoing trunk belongs visually to the row that is already present,
/// but its timing belongs to the next row's arrival.
pub(super) fn tool_connector_continuation(next_progress: Option<f32>) -> f32 {
    next_progress
        .map(|progress| (progress / 0.45).clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

/// Reveal by distance along the elbow and straight leg, so changing the branch
/// length does not introduce a speed jump at their junction.
pub(super) fn activity_branch_points(progress: f32) -> Vec<Point<f32>> {
    let mut path: Vec<_> = (0..=24)
        .map(|step| {
            let t = step as f32 / 24.0;
            point(
                ACTIVITY_BEND_RADIUS * t * t,
                ACTIVITY_BEND_RADIUS * (2.0 * t - t * t),
            )
        })
        .collect();
    path.push(point(
        ACTIVITY_BRANCH_END_X - ACTIVITY_TRUNK_X,
        ACTIVITY_BEND_RADIUS,
    ));
    if progress >= 1.0 {
        return path;
    }
    let lengths: Vec<_> = path
        .windows(2)
        .map(|pair| (pair[1].x - pair[0].x).hypot(pair[1].y - pair[0].y))
        .collect();
    let mut remaining = lengths.iter().sum::<f32>() * progress.clamp(0.0, 1.0);
    let mut visible = vec![path[0]];
    for (pair, length) in path.windows(2).zip(lengths) {
        if remaining <= 0.0 {
            break;
        }
        let t = (remaining / length).min(1.0);
        visible.push(point(
            motion::lerp(pair[0].x, pair[1].x, t),
            motion::lerp(pair[0].y, pair[1].y, t),
        ));
        remaining -= length;
    }
    visible
}

pub(super) fn tool_disclosure_progress(open: bool, fold: FoldState, now: Instant) -> f32 {
    let Some(start) = fold.disclosure_at else {
        return if open { 1.0 } else { 0.0 };
    };
    let raw = now
        .checked_duration_since(start)
        .unwrap_or_default()
        .as_secs_f32()
        / TOOL_FOLD.total().as_secs_f32();
    let progress = TOOL_FOLD.curve.eval(raw);
    if open { progress } else { 1.0 - progress }
}

/// BoardUI's measured recipe: a 300%-wide repeating gradient moves from 200%
/// to -100%. Its 38→50→62% highlight maps to a 36%-of-title shoulder around
/// each peak; adjacent copies sit three title-widths apart. Sampling this by
/// x-coordinate lets the paint clips reproduce the continuous pattern.
pub(super) fn tool_title_shimmer_amount(x: f32, phase: f32) -> f32 {
    let primary_center = -2.5 + phase.clamp(0.0, 1.0) * 6.0;
    (-2..=2)
        .map(|copy| primary_center + copy as f32 * 3.0)
        .map(|center| (1.0 - (x - center).abs() / TOOL_GROUP_SHIMMER_HALF_WIDTH).clamp(0.0, 1.0))
        .fold(0.0, f32::max)
}

pub(super) fn tool_title_shimmer_phase(start: Instant, now: Instant) -> f32 {
    let elapsed = now.checked_duration_since(start).unwrap_or_default();
    (elapsed.as_secs_f32() / TOOL_GROUP_SHIMMER_DURATION.as_secs_f32()).fract()
}
