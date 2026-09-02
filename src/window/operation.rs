//! Coordination for asynchronous source-media operations.
//!
//! `fileops` performs persistent writes. This module owns the identity and
//! applicability rules that decide whether a late result may also update the
//! active Navigation set or displayed media.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::folder::{Destination, FileSnapshot, Navigation, NavigationSetId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct OperationId(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Context {
    id: OperationId,
    path: PathBuf,
    set: NavigationSetId,
    media_generation: u64,
}

impl Context {
    fn matches_set(&self, navigation: &Navigation) -> bool {
        navigation.set_id() == Some(self.set)
    }

    fn matches_media(&self, navigation: &Navigation) -> bool {
        self.matches_set(navigation)
            && navigation.generation() == self.media_generation
            && navigation.current_path() == Some(self.path.as_path())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrashToken(Context);

impl TrashToken {
    pub(super) fn path(&self) -> &Path {
        &self.0.path
    }

    pub(super) fn set(&self) -> NavigationSetId {
        self.0.set
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SaveToken(Context);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UndoId(OperationId);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UndoToken {
    id: OperationId,
    path: PathBuf,
    set: NavigationSetId,
    return_generation: Option<u64>,
}

impl UndoToken {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
struct PendingTrash {
    context: Context,
    return_generation: Option<u64>,
}

#[derive(Debug, Clone)]
struct UndoOffer {
    token: UndoToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UndoDisposition {
    Offered(UndoId),
    NewerOfferPreserved,
    OperationSuperseded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UndoEffect {
    Detached,
    Reconciled(Option<Destination>),
}

#[derive(Debug, Default)]
pub(super) struct Coordinator {
    next: u64,
    trash: HashMap<OperationId, PendingTrash>,
    undo: Option<UndoOffer>,
    undo_in_flight: Option<UndoToken>,
    save: Option<SaveToken>,
}

impl Coordinator {
    fn context(&mut self, navigation: &Navigation, path: &Path) -> Option<Context> {
        let set = navigation.set_id()?;
        if navigation.current_path() != Some(path) {
            return None;
        }
        self.next = self.next.wrapping_add(1);
        Some(Context {
            id: OperationId(self.next),
            path: path.to_path_buf(),
            set,
            media_generation: navigation.generation(),
        })
    }

    pub(super) fn start_trash(
        &mut self,
        navigation: &Navigation,
        path: &Path,
    ) -> Option<TrashToken> {
        // Keep background GIO work bounded even if the user navigates while
        // Trash is pending. One operation is sufficient for the interactive
        // action and keeps completion/Undo ordering straightforward.
        if !self.trash.is_empty()
            || self.undo_in_flight.is_some()
            || self.save.as_ref().is_some_and(|token| token.0.path == path)
        {
            return None;
        }
        let context = self.context(navigation, path)?;
        let token = TrashToken(context.clone());
        self.trash.insert(
            context.id,
            PendingTrash {
                context,
                return_generation: None,
            },
        );
        Some(token)
    }

    pub(super) fn trash_applies(&self, token: &TrashToken, navigation: &Navigation) -> bool {
        self.trash.contains_key(&token.0.id) && token.0.matches_set(navigation)
    }

    /// Record the final media generation after a matching removal has also
    /// been presented. This works whether the monitor or operation result
    /// observes the removal first.
    pub(super) fn observe_removal(
        &mut self,
        set: NavigationSetId,
        path: &Path,
        before_generation: u64,
        after_generation: u64,
        removed_current: bool,
    ) {
        if !removed_current {
            return;
        }
        for pending in self.trash.values_mut() {
            if pending.context.set == set
                && pending.context.path == path
                && pending.context.media_generation == before_generation
            {
                pending.return_generation = Some(after_generation);
            }
        }
    }

    pub(super) fn finish_trash(&mut self, token: TrashToken) -> UndoDisposition {
        let Some(pending) = self.trash.remove(&token.0.id) else {
            return UndoDisposition::OperationSuperseded;
        };
        let undo = UndoToken {
            id: pending.context.id,
            path: pending.context.path,
            set: pending.context.set,
            return_generation: pending.return_generation,
        };
        if self
            .undo
            .as_ref()
            .is_some_and(|current| current.token.id > undo.id)
        {
            return UndoDisposition::NewerOfferPreserved;
        }
        let id = UndoId(undo.id);
        self.undo = Some(UndoOffer { token: undo });
        UndoDisposition::Offered(id)
    }

    pub(super) fn cancel_trash(&mut self, token: &TrashToken) {
        self.trash.remove(&token.0.id);
    }

    pub(super) fn has_undo(&self) -> bool {
        self.undo.is_some()
    }

    pub(super) fn expire_undo(&mut self, id: UndoId) -> bool {
        if self
            .undo
            .as_ref()
            .is_some_and(|offer| offer.token.id == id.0)
        {
            self.undo = None;
            true
        } else {
            false
        }
    }

    pub(super) fn begin_undo(&mut self) -> Option<UndoToken> {
        if self.undo_in_flight.is_some() {
            return None;
        }
        let token = self.undo.take()?.token;
        self.undo_in_flight = Some(token.clone());
        Some(token)
    }

    /// Reconcile a successful restore with the originating Navigation set.
    /// The entry is inserted (or deduplicated) whenever that set is active,
    /// but it is presented only if the user has not navigated since Trash.
    pub(super) fn finish_undo(
        &mut self,
        token: &UndoToken,
        snapshot: FileSnapshot,
        navigation: &mut Navigation,
    ) -> UndoEffect {
        if self.undo_in_flight.as_ref() != Some(token) {
            return UndoEffect::Detached;
        }
        self.undo_in_flight = None;
        if snapshot.path() != token.path || navigation.set_id() != Some(token.set) {
            return UndoEffect::Detached;
        }
        let may_present = token.return_generation == Some(navigation.generation());
        let index = navigation
            .insert(snapshot)
            .or_else(|| navigation.index_of(&token.path));
        let destination = if may_present {
            index.and_then(|index| navigation.select(index))
        } else {
            None
        };
        UndoEffect::Reconciled(destination)
    }

    pub(super) fn cancel_undo(&mut self, token: &UndoToken) {
        if self.undo_in_flight.as_ref() == Some(token) {
            self.undo_in_flight = None;
        }
    }

    pub(super) fn start_save(&mut self, navigation: &Navigation, path: &Path) -> Option<SaveToken> {
        if self.save.is_some()
            || self.undo_in_flight.is_some()
            || self
                .trash
                .values()
                .any(|pending| pending.context.path == path)
        {
            return None;
        }
        let token = SaveToken(self.context(navigation, path)?);
        self.save = Some(token.clone());
        Some(token)
    }

    pub(super) fn is_saving(&self) -> bool {
        self.save.is_some()
    }

    /// Finish the matching save and return the exact destination that may be
    /// reloaded. A stale save still changed its captured path on disk, but it
    /// cannot reload whichever media is current now.
    pub(super) fn finish_save(
        &mut self,
        token: &SaveToken,
        navigation: &Navigation,
    ) -> Option<Destination> {
        if self.save.as_ref() != Some(token) {
            return None;
        }
        self.save = None;
        token
            .0
            .matches_media(navigation)
            .then(|| navigation.current_destination())
            .flatten()
    }

    pub(super) fn cancel_save(&mut self, token: &SaveToken) {
        if self.save.as_ref() == Some(token) {
            self.save = None;
        }
    }

    pub(super) fn clear(&mut self) {
        self.trash.clear();
        self.undo = None;
        self.undo_in_flight = None;
        self.save = None;
    }
}
