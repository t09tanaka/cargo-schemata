// Copyright 2023-2025 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! `--shard k/n`: which of the mutants one part of a divided run takes.

use std::str::FromStr;

use anyhow::{Context, Error, anyhow, ensure};

/// One part of a run that is divided into `n` parts.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Shard {
    /// Which part, from 0.
    pub k: usize,
    /// How many parts.
    pub n: usize,
}

impl FromStr for Shard {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s.split_once('/').ok_or(anyhow!("shard must be k/n"))?;
        let k = parts.0.parse().context("shard k")?;
        let n = parts.1.parse().context("shard n")?;
        ensure!(k < n, "shard k must be less than n"); // implies n>0
        Ok(Shard { k, n })
    }
}

/// How the mutants are divided between the parts (`--sharding`, as in
/// cargo-mutants).
///
/// Both work on the list of mutants as `--list` shows it, after `--in-diff` and
/// the other filters. That order is the same on every machine for the same source
/// tree and options, so the parts of one run agree on who has which mutant.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sharding {
    /// Mutant `i` of the list goes to part `i mod n`. Slow mutants tend to sit
    /// together (in one file), and this spreads them over all the parts.
    #[default]
    RoundRobin,
    /// Consecutive ranges: the first `ceil(total / n)` mutants to part 0, and so on.
    Slice,
}

impl Sharding {
    /// The name in `--sharding`, and in `config.shard.sharding` of a report.
    pub fn name(self) -> &'static str {
        match self {
            Sharding::RoundRobin => "round-robin",
            Sharding::Slice => "slice",
        }
    }

    /// The mode a part's report names; `slice` when it names none (what 0.3.0 wrote).
    pub fn from_name(name: Option<&str>) -> Option<Sharding> {
        match name {
            None | Some("slice") => Some(Sharding::Slice),
            Some("round-robin") => Some(Sharding::RoundRobin),
            Some(_) => None,
        }
    }

    /// Whether position `pos` in a list of `total` mutants belongs to `shard`.
    pub fn owns(self, shard: Shard, pos: usize, total: usize) -> bool {
        self.owner(shard.n, pos, total) == shard.k
    }

    /// The part that position `pos` in a list of `total` mutants belongs to.
    pub fn owner(self, n: usize, pos: usize, total: usize) -> usize {
        match self {
            Sharding::RoundRobin => pos % n,
            Sharding::Slice => pos / total.div_ceil(n),
        }
    }

    /// The mutants of `shard`, in list order.
    pub fn select<T: Clone>(self, shard: Shard, all: &[T]) -> Vec<T> {
        all.iter()
            .enumerate()
            .filter(|&(pos, _)| self.owns(shard, pos, all.len()))
            .map(|(_, m)| m.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_k_over_n() {
        assert_eq!("2/5".parse::<Shard>().unwrap(), Shard { k: 2, n: 5 });
        for (bad, msg) in [
            ("", "shard must be k/n"),
            ("2", "shard must be k/n"),
            ("2/0", "shard k must be less than n"),
            ("5/2", "shard k must be less than n"),
            ("2/2", "shard k must be less than n"),
        ] {
            assert_eq!(bad.parse::<Shard>().unwrap_err().to_string(), msg, "{bad}");
        }
        assert!("a/2".parse::<Shard>().is_err());
        assert!("1/b".parse::<Shard>().is_err());
        assert!("-1/2".parse::<Shard>().is_err());
    }

    fn part(mode: Sharding, k: usize, n: usize, total: usize) -> Vec<usize> {
        let all: Vec<usize> = (0..total).collect();
        mode.select(Shard { k, n }, &all)
    }

    #[test]
    fn slices_are_consecutive_and_cover_everything() {
        let part = |k, n| part(Sharding::Slice, k, n, 10);
        assert_eq!(part(0, 4), [0, 1, 2]);
        assert_eq!(part(1, 4), [3, 4, 5]);
        assert_eq!(part(2, 4), [6, 7, 8]);
        assert_eq!(part(3, 4), [9]);
        // more parts than mutants: the last parts are empty
        assert_eq!(part(9, 12), [9]);
        assert_eq!(part(10, 12), Vec::<usize>::new());
        assert_eq!(part(11, 12), Vec::<usize>::new());
        assert_eq!(part(0, 1), (0..10).collect::<Vec<_>>());
        assert!(
            Sharding::Slice
                .select(Shard { k: 2, n: 3 }, &[0u8; 0])
                .is_empty()
        );
    }

    #[test]
    fn round_robin_deals_the_mutants_out_in_turn() {
        // the example of cargo-mutants
        let part = |k, n| part(Sharding::RoundRobin, k, n, 10);
        assert_eq!(part(0, 4), [0, 4, 8]);
        assert_eq!(part(1, 4), [1, 5, 9]);
        assert_eq!(part(2, 4), [2, 6]);
        assert_eq!(part(3, 4), [3, 7]);
        // more parts than mutants: the last parts are empty
        assert_eq!(part(9, 12), [9]);
        assert_eq!(part(10, 12), Vec::<usize>::new());
        assert_eq!(part(0, 1), (0..10).collect::<Vec<_>>());
        assert!(
            Sharding::RoundRobin
                .select(Shard { k: 2, n: 3 }, &[0u8; 0])
                .is_empty()
        );
    }

    #[test]
    fn every_mutant_has_exactly_one_part_in_both_modes() {
        for mode in [Sharding::RoundRobin, Sharding::Slice] {
            for total in 0..30 {
                for n in 1..35 {
                    let mut seen = vec![0; total];
                    for k in 0..n {
                        for pos in part(mode, k, n, total) {
                            seen[pos] += 1;
                            assert_eq!(mode.owner(n, pos, total), k, "{mode:?} {total} {n}");
                        }
                    }
                    assert!(seen.iter().all(|&c| c == 1), "{mode:?} total {total} n {n}");
                }
            }
        }
    }

    #[test]
    fn round_robin_spreads_a_cluster_of_slow_mutants() {
        // 8 slow mutants in a row at the start of 16: a slice gives them to two parts
        let load = |mode, k| part(mode, k, 4, 16).into_iter().filter(|&p| p < 8).count();
        let loads = |mode| (0..4).map(|k| load(mode, k)).collect::<Vec<_>>();
        assert_eq!(loads(Sharding::Slice), [4, 4, 0, 0]);
        assert_eq!(loads(Sharding::RoundRobin), [2, 2, 2, 2]);
    }

    #[test]
    fn names() {
        assert_eq!(Sharding::default(), Sharding::RoundRobin);
        for m in [Sharding::RoundRobin, Sharding::Slice] {
            assert_eq!(Sharding::from_name(Some(m.name())), Some(m));
        }
        assert_eq!(Sharding::from_name(None), Some(Sharding::Slice));
        assert_eq!(Sharding::from_name(Some("zigzag")), None);
    }
}
