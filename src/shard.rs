// Copyright 2023-2025 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! `--shard k/n`: which of the mutants one part of a divided run takes.

use std::str::FromStr;

use anyhow::{Context, Error, anyhow, ensure};

use crate::mutant::Mutant;
use crate::rewrite::{Placement, placement};

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

/// How the mutants are divided between the parts (`--sharding`).
///
/// All work on the list of mutants as `--list` shows it, after `--in-diff` and
/// the other filters, and on which of them are built one at a time (decided by
/// the kind of mutant and where it is, see [`crate::rewrite::placement`]). Both
/// are the same on every machine for the same source tree and options, so the
/// parts of one run agree on who has which mutant.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sharding {
    /// The mutants that are built one at a time are dealt out in turn first,
    /// and the others continue the turn. Each of the first costs a build of the
    /// package of its own, one after the other, so they decide how long a part
    /// takes; this gives the parts as many of them as it can, and still as many
    /// mutants in all (at most one apart).
    #[default]
    Balanced,
    /// Mutant `i` of the list goes to part `i mod n`. Slow mutants tend to sit
    /// together (in one file), and this spreads them over all the parts.
    RoundRobin,
    /// Consecutive ranges: the first `ceil(total / n)` mutants to part 0, and so on.
    Slice,
}

impl Sharding {
    /// The name in `--sharding`, and in `config.shard.sharding` of a report.
    pub fn name(self) -> &'static str {
        match self {
            Sharding::Balanced => "balanced",
            Sharding::RoundRobin => "round-robin",
            Sharding::Slice => "slice",
        }
    }

    /// The mode a part's report names; `slice` when it names none (what 0.3.0 wrote).
    pub fn from_name(name: Option<&str>) -> Option<Sharding> {
        match name {
            None | Some("slice") => Some(Sharding::Slice),
            Some("round-robin") => Some(Sharding::RoundRobin),
            Some("balanced") => Some(Sharding::Balanced),
            Some(_) => None,
        }
    }

    /// The part that each mutant of the list belongs to, out of `n` parts.
    /// `alone[i]` says whether mutant `i` is built one at a time; only
    /// `balanced` looks at it.
    pub fn owners(self, n: usize, alone: &[bool]) -> Vec<usize> {
        let total = alone.len();
        match self {
            Sharding::RoundRobin => (0..total).map(|pos| pos % n).collect(),
            Sharding::Slice => (0..total).map(|pos| pos / total.div_ceil(n)).collect(),
            Sharding::Balanced => {
                // the others start where the deal of the ones built alone ended
                let mut heavy = 0;
                let mut light = alone.iter().filter(|&&a| a).count();
                alone
                    .iter()
                    .map(|&a| {
                        let turn = if a { &mut heavy } else { &mut light };
                        *turn += 1;
                        (*turn - 1) % n
                    })
                    .collect()
            }
        }
    }

    /// The mutants of `shard`, in list order. `alone` is as for [`Sharding::owners`].
    pub fn select<T: Clone>(self, shard: Shard, all: &[T], alone: &[bool]) -> Vec<T> {
        assert_eq!(all.len(), alone.len(), "one flag for each mutant");
        all.iter()
            .zip(self.owners(shard.n, alone))
            .filter(|&(_, k)| k == shard.k)
            .map(|(m, _)| m.clone())
            .collect()
    }
}

/// For each mutant of the list, whether it is built one at a time: what
/// [`Sharding::owners`] takes.
pub fn alone(mutants: &[Mutant]) -> Vec<bool> {
    mutants
        .iter()
        .map(|m| placement(m) == Placement::Single)
        .collect()
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
        mode.select(Shard { k, n }, &all, &vec![false; total])
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
                .select(Shard { k: 2, n: 3 }, &[0u8; 0], &[])
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
                .select(Shard { k: 2, n: 3 }, &[0u8; 0], &[])
                .is_empty()
        );
    }

    const MODES: [Sharding; 3] = [Sharding::Balanced, Sharding::RoundRobin, Sharding::Slice];

    /// Which mutants of a list of `total` are built alone, from the bits of `mask`.
    fn alone_of(mask: u32, total: usize) -> Vec<bool> {
        (0..total).map(|i| mask >> (i % 32) & 1 == 1).collect()
    }

    #[test]
    fn every_mutant_has_exactly_one_part_in_every_mode() {
        for mode in MODES {
            for total in 0..30 {
                for mask in [0, u32::MAX, 0b1001_0110, 0x0f0f_0f0f] {
                    let alone = alone_of(mask, total);
                    for n in 1..35 {
                        let owners = mode.owners(n, &alone);
                        assert_eq!(owners.len(), total);
                        let all: Vec<usize> = (0..total).collect();
                        let mut seen = vec![0; total];
                        for k in 0..n {
                            for pos in mode.select(Shard { k, n }, &all, &alone) {
                                seen[pos] += 1;
                                assert_eq!(owners[pos], k, "{mode:?} {total} {n}");
                            }
                        }
                        assert!(seen.iter().all(|&c| c == 1), "{mode:?} total {total} n {n}");
                    }
                }
            }
        }
    }

    #[test]
    fn without_mutants_built_alone_balanced_is_round_robin() {
        for total in 0..30 {
            for n in 1..8 {
                let alone = vec![false; total];
                assert_eq!(
                    Sharding::Balanced.owners(n, &alone),
                    Sharding::RoundRobin.owners(n, &alone)
                );
            }
        }
    }

    #[test]
    fn balanced_deals_the_mutants_built_alone_first_and_the_others_after_them() {
        // positions 1, 2, 5, 6 and 9 of 12 are built alone
        let alone = alone_of(0b10_0110_0110, 12);
        let owners = Sharding::Balanced.owners(4, &alone);
        // alone: 1->0, 2->1, 5->2, 6->3, 9->0; the others go on from part 1
        assert_eq!(owners, [1, 0, 1, 2, 3, 2, 3, 0, 1, 0, 2, 3]);
        let alone_in = |k| (0..12).filter(|&p| alone[p] && owners[p] == k).count();
        assert_eq!((0..4).map(alone_in).collect::<Vec<_>>(), [2, 1, 1, 1]);
        let size = |k| owners.iter().filter(|&&o| o == k).count();
        assert_eq!((0..4).map(size).collect::<Vec<_>>(), [3, 3, 3, 3]);
    }

    #[test]
    fn balanced_evens_out_what_round_robin_leaves_uneven() {
        // every fourth mutant from position 1 and every fourth from 3 is built
        // alone: round-robin gives all of them to parts 1 and 3
        let alone: Vec<bool> = (0..40)
            .map(|p| p % 2 == 1 && p % 4 != 1 || p % 8 == 1)
            .collect();
        let loads = |mode: Sharding| {
            let owners = mode.owners(4, &alone);
            (0..4)
                .map(|k| (0..40).filter(|&p| alone[p] && owners[p] == k).count())
                .collect::<Vec<_>>()
        };
        assert_eq!(loads(Sharding::RoundRobin), [0, 5, 0, 10]);
        assert_eq!(loads(Sharding::Balanced), [4, 4, 4, 3]);
        for mode in MODES {
            let owners = mode.owners(4, &alone);
            let sizes: Vec<usize> = (0..4)
                .map(|k| owners.iter().filter(|&&o| o == k).count())
                .collect();
            assert_eq!(sizes, [10, 10, 10, 10], "{mode:?}");
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
        assert_eq!(Sharding::default(), Sharding::Balanced);
        for m in MODES {
            assert_eq!(Sharding::from_name(Some(m.name())), Some(m));
        }
        assert_eq!(Sharding::from_name(None), Some(Sharding::Slice));
        assert_eq!(Sharding::from_name(Some("zigzag")), None);
    }
}
