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

impl Shard {
    /// The positions (in the list of `total` mutants) that belong to this part:
    /// consecutive ranges, the first `ceil(total / n)` to part 0, and so on
    /// (the `slice` method of cargo-mutants, its default).
    pub fn range(self, total: usize) -> std::ops::Range<usize> {
        let chunk_size = total.div_ceil(self.n);
        let start = (self.k * chunk_size).min(total);
        let end = ((self.k + 1) * chunk_size).min(total);
        start..end
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

    #[test]
    fn slices_are_consecutive_and_cover_everything() {
        let all: Vec<usize> = (0..10).collect();
        let part = |k, n| all[Shard { k, n }.range(all.len())].to_vec();
        assert_eq!(part(0, 4), [0, 1, 2]);
        assert_eq!(part(1, 4), [3, 4, 5]);
        assert_eq!(part(2, 4), [6, 7, 8]);
        assert_eq!(part(3, 4), [9]);
        // more parts than mutants: the last parts are empty
        assert_eq!(part(9, 12), [9]);
        assert_eq!(part(10, 12), Vec::<usize>::new());
        assert_eq!(part(11, 12), Vec::<usize>::new());
        assert_eq!(part(4, 12), [4]);
        assert_eq!(part(0, 1), all);
        for n in 1..13 {
            let joined: Vec<usize> = (0..n).flat_map(|k| part(k, n)).collect();
            assert_eq!(joined, all, "n = {n}");
        }
        assert_eq!(Shard { k: 2, n: 3 }.range(0), 0..0);
        assert_eq!(Shard { k: 1, n: 4 }.range(10), 3..6);
    }
}
