//! Interleave files: take pages from each file in turn (A1, B1, A2, B2, …), for a duplex scan
//! whose fronts and backs came out as two files. The result is a list of runs that
//! [`crate::combine_grouped_with`] turns into one document, each file copied once.

use crate::OrganizeError;

/// The runs that interleave `page_lists` (each file's chosen pages, 0-based, in order): up to
/// `per_turn` pages from the first file, then from the second, and so on, round and round. A
/// file whose `reverse` entry is `true` gives its pages last to first (the backs of a duplex scan
/// come out in reverse order); a missing entry means `false`. When a file runs out, the others
/// go on taking turns, so the rest of the last file follows in order.
///
/// Each run is `(file, pages)`: `file` is the index into `page_lists`. Runs are never empty, and a
/// run is merged with the one before when both come from the same file, so a long tail is one
/// run. Every listed page appears exactly once. Empty lists take no turns.
///
/// Errors: `per_turn` is 0.
pub fn interleave_runs(page_lists: &[Vec<usize>], per_turn: usize, reverse: &[bool]) -> Result<Vec<(usize, Vec<usize>)>, OrganizeError> {
    if per_turn == 0 {
        return Err(OrganizeError::Invalid("pages per turn must be at least 1".into()));
    }
    // Each file's pages in the order they are taken, and how many have been taken.
    let ordered: Vec<Vec<usize>> = page_lists
        .iter()
        .enumerate()
        .map(|(i, pages)| if reverse.get(i).copied().unwrap_or(false) { pages.iter().rev().copied().collect() } else { pages.clone() })
        .collect();
    let mut taken: Vec<usize> = vec![0; ordered.len()];
    // The files still to take a turn, in order; a file that runs out leaves, so the loop's
    // work is bounded by the number of pages and turns, however many empty files there are.
    let mut active: Vec<usize> = (0..ordered.len()).filter(|i| ordered.get(*i).is_some_and(|p| !p.is_empty())).collect();
    let mut runs: Vec<(usize, Vec<usize>)> = Vec::new();
    while !active.is_empty() {
        active.retain(|&file| {
            let (Some(pages), Some(at)) = (ordered.get(file), taken.get_mut(file)) else { return false };
            let end = at.saturating_add(per_turn).min(pages.len());
            let turn = pages.get(*at..end).unwrap_or_default();
            *at = end;
            match runs.last_mut() {
                Some((last, run)) if *last == file => run.extend_from_slice(turn),
                _ if turn.is_empty() => {}
                _ => runs.push((file, turn.to_vec())),
            }
            end < pages.len()
        });
    }
    Ok(runs)
}
