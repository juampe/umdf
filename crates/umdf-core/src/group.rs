//! Union-find grouping and "which copy should I keep?" policies.

use std::collections::HashMap;

use crate::config::{KeepPolicy, ScanConfig};
use crate::fingerprint;
use crate::model::{Group, Match, MediaItem};
use crate::originality;

/// Similarity of two items with the same rule used by the comparison stage.
fn direct_score(
    items: &[MediaItem],
    a: usize,
    b: usize,
    cfg: &ScanConfig,
) -> Option<f32> {
    let ai = &items[a];
    let bi = &items[b];
    if ai.content_hash.is_some() && ai.content_hash == bi.content_hash {
        return Some(100.0);
    }
    if ai.media_signature.is_some() && ai.media_signature == bi.media_signature {
        return Some(99.0);
    }
    if let (Some(ea), Some(eb)) = (ai.embedding.as_ref(), bi.embedding.as_ref()) {
        return Some(fingerprint::cosine_percent(ea, eb));
    }
    let (Some(fa), Some(fb)) = (ai.fingerprint.as_ref(), bi.fingerprint.as_ref()) else {
        return None;
    };
    fingerprint::similarity(fa, fb, cfg.metric, cfg.pixel_tolerance)
}

/// A member joins a group only when it matches the chosen keep directly.
/// Union-find alone allows chains (A~B, B~C) to put visually unrelated A and C
/// in the same group, which looked like a 100 % false positive in the UI.
fn member_matches_keep(
    items: &[MediaItem],
    keep: usize,
    other: usize,
    cfg: &ScanConfig,
) -> bool {
    let ai = &items[keep];
    let bi = &items[other];
    if ai.content_hash.is_some() && ai.content_hash == bi.content_hash {
        return true;
    }
    if ai.media_signature.is_some() && ai.media_signature == bi.media_signature {
        return true;
    }
    let threshold = if ai.embedding.is_some() && bi.embedding.is_some() {
        cfg.embeddings
            .as_ref()
            .map(|e| e.threshold * 100.0)
            .unwrap_or(cfg.similarity_threshold)
    } else {
        cfg.similarity_threshold
    };
    if ai.embedding.is_some() && bi.embedding.is_some() {
        return direct_score(items, keep, other, cfg)
            .map(|score| score >= threshold)
            .unwrap_or(true);
    }
    let (Some(fa), Some(fb)) = (ai.fingerprint.as_ref(), bi.fingerprint.as_ref()) else {
        // No fingerprint to check (for example an exact video match): keep it.
        return true;
    };
    fingerprint::similarity_at_least(
        fa,
        fb,
        cfg.metric,
        cfg.pixel_tolerance,
        threshold,
    )
    .map(|score| score >= threshold)
    .unwrap_or(false)
}

struct DisjointSet {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl DisjointSet {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            let root = self.find(self.parent[x]);
            self.parent[x] = root;
        }
        self.parent[x]
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        if self.rank[ra] < self.rank[rb] {
            self.parent[ra] = rb;
        } else if self.rank[ra] > self.rank[rb] {
            self.parent[rb] = ra;
        } else {
            self.parent[rb] = ra;
            self.rank[ra] += 1;
        }
    }
}

/// Build connected components from the match graph and apply the keep policy.
pub fn build_groups(items: &[MediaItem], matches: &[Match], cfg: &ScanConfig) -> Vec<Group> {
    if items.is_empty() {
        return Vec::new();
    }
    let mut ds = DisjointSet::new(items.len());
    for m in matches {
        ds.union(m.a, m.b);
    }

    let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
    for item in items {
        let root = ds.find(item.id);
        let members = buckets.entry(root).or_default();
        members.push(item.id);
    }

    let mut groups: Vec<Group> = buckets
        .into_values()
        .filter(|m| m.len() > 1)
        .filter_map(|mut members| {
            let assessments = originality::assess_group(items, &members);
            match cfg.keep_policy {
                KeepPolicy::MostLikelyOriginal => members.sort_by(|a, b| {
                    let ca = assessments.get(a).map(|x| x.confidence).unwrap_or(0.0);
                    let cb = assessments.get(b).map(|x| x.confidence).unwrap_or(0.0);
                    cb.partial_cmp(&ca)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| items[*a].file_name().cmp(&items[*b].file_name()))
                }),
                other => members.sort_by(|a, b| compare_for_keep(&items[*a], &items[*b], other)),
            }
            let keep = members[0];
            // Break transitive chains: only direct matches to the keep stay.
            members.retain(|member| *member == keep || member_matches_keep(items, keep, *member, cfg));
            if members.len() <= 1 {
                return None;
            }
            let remove: Vec<usize> = members[1..].to_vec();
            let member_scores: Vec<f32> = remove
                .iter()
                .map(|id| direct_score(items, keep, *id, cfg).unwrap_or(0.0))
                .collect();
            let reclaimable = remove.iter().map(|id| items[*id].size).sum();
            let max_similarity = member_scores.iter().copied().fold(0.0f32, f32::max);
            let assessment = assessments.get(&keep);
            Some(Group {
                number: 0,
                max_similarity,
                keep_confidence: assessment.map(|a| a.confidence).unwrap_or(100.0),
                keep_reasons: assessment.map(|a| a.reasons.clone()).unwrap_or_default(),
                keep,
                items: members,
                remove,
                member_scores,
                reclaimable_bytes: reclaimable,
            })
        })
        .collect();

    // Stable, human-friendly numbering: biggest reclaimable groups first.
    groups.sort_by(|a, b| {
        b.reclaimable_bytes
            .cmp(&a.reclaimable_bytes)
            .then_with(|| items[a.keep].path.cmp(&items[b.keep].path))
    });
    for (i, g) in groups.iter_mut().enumerate() {
        g.number = i + 1;
    }
    groups
}

/// Order two items best-first for the given policy.
pub fn compare_for_keep(a: &MediaItem, b: &MediaItem, policy: KeepPolicy) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match policy {
        // Handled by `originality::assess_group`; this fallback keeps the
        // function total for callers that use it directly.
        KeepPolicy::MostLikelyOriginal => b.size.cmp(&a.size),
        KeepPolicy::Largest => b.size.cmp(&a.size),
        KeepPolicy::HighestResolution => b
            .megapixels()
            .partial_cmp(&a.megapixels())
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.size.cmp(&a.size)),
        KeepPolicy::Newest => b.effective_date().cmp(&a.effective_date()),
        KeepPolicy::Oldest => a.effective_date().cmp(&b.effective_date()),
        KeepPolicy::FirstName => a.file_name().cmp(&b.file_name()),
        KeepPolicy::ShortestPath => a
            .path
            .as_os_str()
            .len()
            .cmp(&b.path.as_os_str().len()),
    }
}

/// Human readable explanation used in the CLI/UI.
pub fn policy_label(policy: KeepPolicy) -> &'static str {
    match policy {
        KeepPolicy::MostLikelyOriginal => "most likely original",
        KeepPolicy::Largest => "largest file",
        KeepPolicy::HighestResolution => "highest resolution",
        KeepPolicy::Newest => "newest",
        KeepPolicy::Oldest => "oldest",
        KeepPolicy::FirstName => "first name",
        KeepPolicy::ShortestPath => "shortest path",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Fingerprint, MatchKind, MediaKind};
    use std::path::PathBuf;

    fn item(id: usize, name: &str, size: u64) -> MediaItem {
        let mut it = MediaItem::new(id, PathBuf::from(name), MediaKind::Image, size);
        it.width = Some(100);
        it.height = Some(100);
        it
    }

    #[test]
    fn chains_merge_into_one_group() {
        let items = vec![item(0, "a.jpg", 10), item(1, "b.jpg", 20), item(2, "c.jpg", 30)];
        let matches = vec![
            Match { a: 0, b: 1, similarity: 95.0, kind: MatchKind::Similar },
            Match { a: 1, b: 2, similarity: 96.0, kind: MatchKind::Similar },
        ];
        let cfg = ScanConfig { keep_policy: KeepPolicy::Largest, ..Default::default() };
        let groups = build_groups(&items, &matches, &cfg);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].keep, 2);
        assert_eq!(groups[0].remove, vec![1, 0]);
    }

    #[test]
    fn a_chain_does_not_put_unrelated_items_in_the_same_group() {
        let a_values: Vec<i16> = (0..1024)
            .map(|i| if i % 2 == 0 { 50 } else { -50 })
            .collect();
        let c_values: Vec<i16> = (0..1024)
            .map(|i| ((i * 13) % 80) as i16 - 40)
            .collect();
        let fingerprint = |values: Vec<i16>| Fingerprint {
            size: 32,
            mean: 100,
            values,
        };
        let mut a = item(0, "a.jpg", 10);
        a.fingerprint = Some(fingerprint(a_values.clone()));
        let mut b = item(1, "b.jpg", 20);
        // B is a real copy of A.
        b.fingerprint = Some(fingerprint(a_values));
        let mut c = item(2, "c.jpg", 30);
        c.fingerprint = Some(fingerprint(c_values));
        let items = vec![a, b, c];
        let matches = vec![
            Match { a: 0, b: 1, similarity: 100.0, kind: MatchKind::Similar },
            // The comparison accepted B~C, which would chain A and C together.
            Match { a: 1, b: 2, similarity: 90.0, kind: MatchKind::Similar },
        ];
        let cfg = ScanConfig {
            keep_policy: KeepPolicy::FirstName,
            ..Default::default()
        };
        let groups = build_groups(&items, &matches, &cfg);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].keep, 0);
        assert_eq!(groups[0].items, vec![0, 1]);
        assert!(!groups[0].items.contains(&2));
        assert_eq!(groups[0].member_scores.len(), groups[0].remove.len());
    }

    #[test]
    fn leave_one_keeps_a_member() {
        let items = vec![item(0, "a.jpg", 10), item(1, "b.jpg", 10)];
        let matches = vec![Match { a: 0, b: 1, similarity: 100.0, kind: MatchKind::Exact }];
        let cfg = ScanConfig { keep_policy: KeepPolicy::Largest, leave_one: true, ..Default::default() };
        let groups = build_groups(&items, &matches, &cfg);
        assert_eq!(groups[0].remove.len(), 1);
        assert!(!groups[0].remove.contains(&groups[0].keep));
    }
}
