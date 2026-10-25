//! Fixed producer-block storage and original mailbox cutoff draining.
//! These remove two dynamic Vec allocations. Command/reply heaps and Tokio
//! channel/task allocator layouts remain separate, incomplete inventory.
use super::*;

// One open progress batch, exit, health and diagnostics, plus the two
// independently prearmed physical-flow barriers. No event may be discarded.
const PRODUCER_BLOCK_CAPACITY: usize = 4 + ROLLING_PRODUCER_FLOW_BARRIER_CAPACITY;

#[cfg_attr(test, derive(Clone, Debug))]
pub(super) struct RollingProducerIngressBlocks {
    slots: [Option<RollingProducerIngressBlock>; PRODUCER_BLOCK_CAPACITY],
    len: usize,
}

impl RollingProducerIngressBlocks {
    pub(super) fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            len: 0,
        }
    }

    pub(super) fn push(&mut self, block: RollingProducerIngressBlock) {
        // A violated producer cardinality must stop this owner and retain its
        // admission, rather than silently truncating the original evidence.
        assert!(
            self.len < PRODUCER_BLOCK_CAPACITY,
            "producer ingress exceeded its fixed original slots"
        );
        self.slots[self.len] = Some(block);
        self.len += 1;
    }

    pub(super) fn sort_by_order(&mut self) {
        // Stable insertion sort of at most six inline entries, with no sort
        // workspace allocation and the same equal-key order as the old Vec.
        for end in 1..self.len {
            let mut index = end;
            while index != 0
                && self.slots[index - 1].as_ref().unwrap().order_key()
                    > self.slots[index].as_ref().unwrap().order_key()
            {
                self.slots.swap(index - 1, index);
                index -= 1;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn as_slice(&self) -> &[Option<RollingProducerIngressBlock>] {
        &self.slots[..self.len]
    }

    #[cfg(test)]
    pub(super) fn iter(&self) -> impl Iterator<Item = &RollingProducerIngressBlock> {
        self.slots[..self.len]
            .iter()
            .map(|slot| slot.as_ref().expect("occupied producer slot"))
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
impl std::ops::Index<usize> for RollingProducerIngressBlocks {
    type Output = RollingProducerIngressBlock;
    fn index(&self, index: usize) -> &Self::Output {
        self.slots[..self.len][index]
            .as_ref()
            .expect("occupied producer slot")
    }
}

impl IntoIterator for RollingProducerIngressBlocks {
    type Item = RollingProducerIngressBlock;
    type IntoIter =
        std::iter::Flatten<std::array::IntoIter<Option<Self::Item>, PRODUCER_BLOCK_CAPACITY>>;
    fn into_iter(self) -> Self::IntoIter {
        self.slots.into_iter().flatten()
    }
}

#[cfg(test)]
impl<const N: usize> From<[RollingProducerIngressBlock; N]> for RollingProducerIngressBlocks {
    fn from(blocks: [RollingProducerIngressBlock; N]) -> Self {
        let mut result = Self::new();
        for block in blocks {
            result.push(block);
        }
        result
    }
}

/// Consume exactly the queue prefix captured under the publication fence.
/// New commands remain in the original FIFO behind the executor-loss cutoff.
/// Neither a second queue nor a replacement receiver is created.
pub(super) async fn next_before_cutoff<T>(
    receiver: &mut tokio::sync::mpsc::Receiver<T>,
    remaining: &mut usize,
) -> Option<T> {
    if *remaining == 0 {
        return None;
    }
    let command = receiver
        .recv()
        .await
        .expect("original sole receiver retains every counted cutoff command");
    *remaining -= 1;
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(sequence: u64, at: Instant) -> RollingProducerIngressBlock {
        RollingProducerIngressBlock::Progress(ProgressCoverageBatch::new(
            PublishedProgress {
                sequence,
                published_at: at,
                observation: RollingProducerProgressObservation {
                    producer_attempt: 1,
                    out_time_ms: Some(sequence as i64),
                    speed_milli: None,
                    recent_speed_milli: None,
                    observed_at: at,
                },
            },
            Duration::from_secs(5),
        ))
    }

    #[test]
    fn fixed_producer_blocks_keep_all_six_slots_in_stable_order() {
        let now = Instant::now();
        let mut blocks = RollingProducerIngressBlocks::new();
        for sequence in (1..=6).rev() {
            blocks.push(progress(sequence, now));
        }
        assert_eq!(
            blocks.as_slice().len(),
            4 + ROLLING_PRODUCER_FLOW_BARRIER_CAPACITY
        );
        blocks.sort_by_order();
        assert_eq!(
            blocks
                .into_iter()
                .map(|block| block.order_key().0)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn original_ingress_fold_keeps_four_sticky_slots_and_both_flow_barriers() {
        let now = Instant::now();
        let barrier = |sequence| SequencedProducerBarrier {
            preceding_progress: None,
            event: SequencedProducerEvent {
                sequence,
                published_at: now,
                event: RollingProducerEvent::Exit(RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: true,
                    code: Some(0),
                    signal: None,
                    observed_at: now,
                }),
            },
        };
        // Synthetic payloads exercise slot cardinality and ordering only.
        let RollingProducerIngressBlock::Progress(open) = progress(6, now) else {
            unreachable!()
        };
        let mut state = RollingProducerIngressState {
            progress: Some(open),
            exit: Some(barrier(5)),
            health: Some(barrier(4)),
            diagnostics: Some(barrier(3)),
            flow: [barrier(2), barrier(1)].into_iter().collect(),
            ..RollingProducerIngressState::default()
        };
        let blocks = RollingProducerIngress::take_blocks(&mut state);
        assert_eq!(
            blocks
                .into_iter()
                .map(|block| block.order_key().0)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
        assert!(RollingProducerIngress::take_blocks(&mut state).is_empty());
    }

    #[test]
    #[should_panic(expected = "producer ingress exceeded its fixed original slots")]
    fn fixed_producer_blocks_refuse_overflow_instead_of_dropping_original_evidence() {
        let now = Instant::now();
        let mut blocks = RollingProducerIngressBlocks::new();
        for sequence in 0..=6 {
            blocks.push(progress(sequence, now));
        }
    }

    #[tokio::test]
    async fn streamed_loss_cutoff_leaves_later_publications_in_the_original_mailbox() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        sender.send(10).await.unwrap();
        sender.send(20).await.unwrap();
        let mut preceding = receiver.len();
        sender.send(30).await.unwrap();
        assert_eq!(
            next_before_cutoff(&mut receiver, &mut preceding).await,
            Some(10)
        );
        // A new publisher may append as the old prefix is handled.
        sender.send(40).await.unwrap();
        assert_eq!(
            next_before_cutoff(&mut receiver, &mut preceding).await,
            Some(20)
        );
        assert_eq!(
            next_before_cutoff(&mut receiver, &mut preceding).await,
            None
        );
        assert_eq!(receiver.recv().await, Some(30));
        assert_eq!(receiver.recv().await, Some(40));
    }
}
