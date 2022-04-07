use crate::app::AppRequest;

use anyhow::Result;

use std::{collections::VecDeque, sync::mpsc::Receiver};

pub(crate) struct PriorityQueue {
    rx: Receiver<AppRequest>,
    output_queue: VecDeque<AppRequest>,
}

impl PriorityQueue {
    pub(crate) fn new(rx: Receiver<AppRequest>) -> PriorityQueue {
        PriorityQueue {
            rx,
            output_queue: Default::default(),
        }
    }

    pub(crate) fn recv(&mut self) -> Result<AppRequest> {
        while let Ok(item) = self.rx.try_recv() {
            if let AppRequest::GetCommitGraph { viewer_id, .. } = &item {
                let output_queue = std::mem::take(&mut self.output_queue);
                let new_id = &viewer_id;
                self.output_queue = output_queue
                    .into_iter()
                    .filter(|existing_item| {
                        if let AppRequest::GetCommitGraph { viewer_id, .. } = &existing_item {
                            &viewer_id != new_id
                        } else {
                            true
                        }
                    })
                    .collect()
            }
            self.output_queue.push_back(item);
        }

        if self.output_queue.is_empty() {
            Ok(self.rx.recv()?)
        } else {
            Ok(self.output_queue.pop_front().unwrap())
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{app::ViewState, git::ReferenceId};
    use std::sync::mpsc;

    macro_rules! is_enum_variant {
        ($v:expr, $p:pat) => {
            if let $p = $v {
                true
            } else {
                false
            }
        };
    }

    #[test]
    fn no_reorder() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut q = PriorityQueue::new(rx);

        tx.send(AppRequest::OpenRepo("1".into()))?;
        tx.send(AppRequest::Refresh)?;
        tx.send(AppRequest::GetCommitGraph {
            expected_repo: "1".into(),
            viewer_id: "Viewer_1".into(),
            view_state: Default::default(),
        })?;

        assert!(is_enum_variant!(q.recv()?, AppRequest::OpenRepo(_)));
        assert!(is_enum_variant!(q.recv()?, AppRequest::Refresh));
        assert!(is_enum_variant!(
            q.recv()?,
            AppRequest::GetCommitGraph { .. }
        ));

        Ok(())
    }

    #[test]
    fn ignore_multiple_commit_graphs() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut q = PriorityQueue::new(rx);

        tx.send(AppRequest::GetCommitGraph {
            expected_repo: "1".into(),
            viewer_id: "Viewer_1".into(),
            view_state: ViewState {
                selected_references: FromIterator::from_iter([ReferenceId::head()]),
            },
        })?;
        tx.send(AppRequest::GetCommitGraph {
            expected_repo: "1".into(),
            viewer_id: "Viewer_1".into(),
            view_state: ViewState {
                selected_references: Default::default(),
            },
        })?;
        tx.send(AppRequest::GetCommitGraph {
            expected_repo: "1".into(),
            viewer_id: "Viewer_1".into(),
            view_state: ViewState {
                selected_references: FromIterator::from_iter([ReferenceId::LocalBranch(
                    "master".into(),
                )]),
            },
        })?;

        if let AppRequest::GetCommitGraph { view_state, .. } = q.recv()? {
            assert_eq!(
                view_state.selected_references,
                FromIterator::from_iter([ReferenceId::LocalBranch("master".into())])
            )
        } else {
            assert!(false);
        }

        Ok(())
    }
}
