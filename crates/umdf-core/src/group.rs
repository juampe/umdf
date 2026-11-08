//! Union-find grouping and "which copy should I keep?" policies.

use std::collections::HashMap;

use crate::config::{KeepPolicy, ScanConfig};
use crate::model::{Group, Match, MediaItem};
use crate::originality;

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
    let mut max_sim: HashMap<usize, f32> = HashMap::new();
    for m in matches {
        let root = ds.find(m.a);
        max_sim
            .entry(root)
            .and_modify(|s| *s = s.max(m.similarity))
            .or_insert(m.similarity);
    }
    for item in items {
        let root = ds.find(item.id);
        let members = buckets.entry(root).or_default();
        members.push(item.id);
    }

    let mut groups: Vec<Group> = buckets
        .into_values()
        .filter(|m| m.len() > 1)
        .map(|mut members| {
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
            let mut remove: Vec<usize> = members[1..].to_vec();
            if cfg.leave_one && remove.len() == members.len() {
                remove.pop();
            }
            let reclaimable = remove.iter().map(|id| items[*id].size).sum();
            let root = ds.find(keep);
            let assessment = assessments.get(&keep);
            Group {
                number: 0,
                max_similarity: max_sim.get(&root).copied().unwrap_or(100.0),
                keep_confidence: assessment.map(|a| a.confidence).unwrap_or(100.0),
                keep_reasons: assessment.map(|a| a.reasons.clone()).unwrap_or_default(),
                keep,
                items: members,
                remove,
                reclaimable_bytes: reclaimable,
            }
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
    use crate::model::{MatchKind, MediaKind};
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
    fn leave_one_keeps_a_member() {
        let items = vec![item(0, "a.jpg", 10), item(1, "b.jpg", 10)];
        let matches = vec![Match { a: 0, b: 1, similarity: 100.0, kind: MatchKind::Exact }];
        let cfg = ScanConfig { keep_policy: KeepPolicy::Largest, leave_one: true, ..Default::default() };
        let groups = build_groups(&items, &matches, &cfg);
        assert_eq!(groups[0].remove.len(), 1);
        assert!(!groups[0].remove.contains(&groups[0].keep));
    }
}
