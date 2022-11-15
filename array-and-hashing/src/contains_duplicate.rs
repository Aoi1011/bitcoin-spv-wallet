use std::collections::{HashMap, HashSet};

#[allow(dead_code)]
pub fn contains_duplicate(nums: Vec<i32>) -> bool {
    let contain = false;

    let mut map = HashMap::new();

    for num in nums {
        if map.contains_key(&num) {
            return true;
        }
        map.insert(num, true);
    }

    contain
}

#[allow(dead_code)]
pub fn contains_duplicate_1(nums: Vec<i32>) -> bool {
    let mut set = HashSet::new();

    for num in nums {
        if set.contains(&num) {
            return true;
        }

        set.insert(num);
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contains_duplicate() {
        let nums = vec![1, 2, 3, 1];
        assert!(contains_duplicate_1(nums));
    }

    #[test]
    fn test_does_not_contains_duplicate() {
        let nums = vec![1, 2, 3, 4];

        assert!(!contains_duplicate_1(nums));
    }

    #[test]
    fn test_many_number_test_case() {
        let nums = vec![1, 1, 1, 3, 3, 4, 3, 2, 4, 2];
        assert!(contains_duplicate_1(nums));
    }
}