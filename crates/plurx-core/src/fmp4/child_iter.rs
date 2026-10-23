use super::{peek_box_before, Child, Fmp4Error};

/// Stream child-box ranges without allocating a `Vec` for the container.
///
/// A trailing short header (including an incomplete extended header) is
/// ignored exactly as it is by the legacy `children` helper. A declared box
/// that exceeds its parent remains an error from `peek_box_before`.
pub(super) fn iter_children(payload: &[u8]) -> ChildIter<'_> {
    ChildIter { payload, pos: 0 }
}

pub(super) struct ChildIter<'a> {
    payload: &'a [u8],
    pos: usize,
}

impl Iterator for ChildIter<'_> {
    type Item = Result<Child, Fmp4Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.payload.len().saturating_sub(self.pos) < 8 {
            return None;
        }
        let header = match peek_box_before(self.payload, self.pos, Some(self.payload.len())) {
            Ok(Some(header)) => header,
            // Legacy `children` silently stops on a truncated extended header.
            Ok(None) => {
                self.pos = self.payload.len();
                return None;
            }
            Err(error) => {
                self.pos = self.payload.len();
                return Some(Err(error));
            }
        };
        let Some(end) = self.pos.checked_add(header.size) else {
            self.pos = self.payload.len();
            return Some(Err(Fmp4Error::Malformed("box size overflows".into())));
        };
        if end > self.payload.len() {
            self.pos = self.payload.len();
            return Some(Err(Fmp4Error::Malformed(format!(
                "box {} runs past its parent",
                super::fourcc(header.kind()),
            ))));
        }
        let start = self.pos + header.header_len;
        self.pos = end;
        Some(Ok((header, start, end)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_iterator_preserves_short_trailing_bytes() {
        let mut payload = vec![0, 0, 0, 8, b'f', b'r', b'e', b'e'];
        payload.extend_from_slice(&[1, 2, 3]);
        let children = iter_children(&payload)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(children.len(), 1);
        assert!(payload[children[0].1..children[0].2].is_empty());
    }

    #[test]
    fn child_iterator_reports_a_box_that_runs_past_its_parent() {
        let payload = [0, 0, 0, 16, b'f', b'r', b'e', b'e'];
        assert!(matches!(iter_children(&payload).next(), Some(Err(_))));
    }

    #[test]
    fn child_iterator_preserves_truncated_extended_header_termination() {
        let payload = [0, 0, 0, 1, b'f', b'r', b'e', b'e'];
        assert!(iter_children(&payload).next().is_none());
    }
}
