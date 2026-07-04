use std::ops::Range;

use fwob_core::{Key, KeyType};

use crate::{Error, Reader, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySelector {
    All,
    Exact(Key),
    From(Key),
    Through(Key),
    Between { first: Key, last: Key },
}

impl KeySelector {
    pub fn parse(value: &str, key_type: KeyType) -> Result<Self> {
        let Some((first, last)) = value.split_once("..") else {
            return Ok(Self::Exact(Key::parse(key_type, value)?));
        };
        if last.contains("..") {
            return Err(Error::InvalidSelector(value.to_owned()));
        }
        match (first.is_empty(), last.is_empty()) {
            // A bare `..` is rejected: selecting everything is expressed by omitting selectors
            // entirely (all-by-default), which also avoids the ambiguity with the `..` parent
            // directory. Half-open (`FIRST..`, `..LAST`) and closed (`FIRST..LAST`) ranges remain.
            (true, true) => Err(Error::InvalidSelector(value.to_owned())),
            (false, true) => Ok(Self::From(Key::parse(key_type, first)?)),
            (true, false) => Ok(Self::Through(Key::parse(key_type, last)?)),
            (false, false) => {
                let first = Key::parse(key_type, first)?;
                let last = Key::parse(key_type, last)?;
                if first > last {
                    return Err(Error::ReversedSelector { first, last });
                }
                Ok(Self::Between { first, last })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSelection {
    ranges: Vec<Range<u64>>,
    frame_count: u64,
}

impl FrameSelection {
    pub fn resolve(reader: &mut Reader, selectors: &[KeySelector]) -> Result<Self> {
        #[allow(clippy::single_range_in_vec_init)]
        let mut ranges = if selectors.is_empty() {
            vec![0..reader.frame_count()]
        } else {
            let mut ranges = Vec::with_capacity(selectors.len());
            for selector in selectors {
                let range = match *selector {
                    KeySelector::All => 0..reader.frame_count(),
                    KeySelector::Exact(key) => reader.equal_range(key)?,
                    KeySelector::From(key) => reader.lower_bound(key)?..reader.frame_count(),
                    KeySelector::Through(key) => 0..reader.upper_bound(key)?,
                    KeySelector::Between { first, last } => {
                        reader.lower_bound(first)?..reader.upper_bound(last)?
                    }
                };
                if !range.is_empty() {
                    ranges.push(range);
                }
            }
            ranges
        };

        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        let mut merged: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(last) = merged.last_mut() {
                if range.start <= last.end {
                    last.end = last.end.max(range.end);
                    continue;
                }
            }
            merged.push(range);
        }
        let frame_count = merged.iter().map(|range| range.end - range.start).sum();
        Ok(Self {
            ranges: merged,
            frame_count,
        })
    }

    pub fn ranges(&self) -> &[Range<u64>] {
        &self.ranges
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn first_index(&self) -> Option<u64> {
        self.ranges.first().map(|range| range.start)
    }

    pub fn end_index(&self) -> Option<u64> {
        self.ranges.last().map(|range| range.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_open_closed_and_unbounded_selectors() {
        assert_eq!(
            KeySelector::parse("10", KeyType::I32).unwrap(),
            KeySelector::Exact(Key::I32(10))
        );
        assert_eq!(
            KeySelector::parse("10..", KeyType::I32).unwrap(),
            KeySelector::From(Key::I32(10))
        );
        assert_eq!(
            KeySelector::parse("..20", KeyType::I32).unwrap(),
            KeySelector::Through(Key::I32(20))
        );
        assert_eq!(
            KeySelector::parse("10..20", KeyType::I32).unwrap(),
            KeySelector::Between {
                first: Key::I32(10),
                last: Key::I32(20),
            }
        );
        // A bare `..` is no longer a selector: select everything by omitting selectors instead.
        assert!(KeySelector::parse("..", KeyType::I32).is_err());
        assert!(KeySelector::parse("20..10", KeyType::I32).is_err());
        assert!(KeySelector::parse("1..2..3", KeyType::I32).is_err());
    }
}
