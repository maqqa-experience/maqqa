/// Two Sum — stub for vibe coding eval.
pub fn two_sum(nums: &[i32], target: i32) -> [usize; 2] {
    unimplemented!("implement two_sum")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(pair: [usize; 2]) -> [usize; 2] {
        let mut p = pair;
        p.sort();
        p
    }

    #[test]
    fn example_1() {
        assert_eq!(sorted(two_sum(&[2, 7, 11, 15], 9)), [0, 1]);
    }

    #[test]
    fn example_2() {
        assert_eq!(sorted(two_sum(&[3, 2, 4], 6)), [1, 2]);
    }

    #[test]
    fn duplicates() {
        assert_eq!(sorted(two_sum(&[3, 3], 6)), [0, 1]);
    }
}
