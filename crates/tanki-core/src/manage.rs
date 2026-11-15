//! Phase 5 envelope: management screens over existing core plumbing
//! (PLAN 11, Phase 5). `Command::Manage(ManageCommand)` answers
//! `Response::Manage(ManageResponse)`. Each stream owns one module with its
//! own command/response enums; see `docs/implementation/phase-5-interfaces.md`.
use crate::{
    connect::{ConnectCommand, ConnectResponse},
    deck_options::{DeckOptionsCommand, DeckOptionsResponse},
    deck_tools::{DeckToolsCommand, DeckToolsResponse},
    maintenance::{MaintenanceCommand, MaintenanceResponse},
    notetypes::{NotetypeCommand, NotetypeResponse},
    stats::{StatsCommand, StatsResponse},
    transfer::{TransferCommand, TransferResponse},
    OpChanges, Result,
};
use anki::collection::Collection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ManageCommand {
    DeckOptions(DeckOptionsCommand),
    DeckTools(DeckToolsCommand),
    Notetypes(NotetypeCommand),
    Stats(StatsCommand),
    Transfer(TransferCommand),
    Maintenance(MaintenanceCommand),
    /// Phase 6: the AnkiConnect-compatible API.
    Connect(ConnectCommand),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ManageResponse {
    DeckOptions(DeckOptionsResponse),
    DeckTools(DeckToolsResponse),
    Notetypes(NotetypeResponse),
    Stats(StatsResponse),
    Transfer(TransferResponse),
    Maintenance(MaintenanceResponse),
    Connect(ConnectResponse),
}

impl ManageResponse {
    /// The changes a mutating command made; the actor broadcasts these.
    pub fn changes(&self) -> Option<&OpChanges> {
        match self {
            Self::DeckOptions(r) => r.changes(),
            Self::DeckTools(r) => r.changes(),
            Self::Notetypes(r) => r.changes(),
            Self::Stats(r) => r.changes(),
            Self::Transfer(r) => r.changes(),
            Self::Maintenance(r) => r.changes(),
            Self::Connect(r) => r.changes(),
        }
    }
}

pub(crate) fn execute(col: &mut Collection, command: ManageCommand) -> Result<ManageResponse> {
    Ok(match command {
        ManageCommand::DeckOptions(c) => {
            ManageResponse::DeckOptions(crate::deck_options::execute(col, c)?)
        }
        ManageCommand::DeckTools(c) => {
            ManageResponse::DeckTools(crate::deck_tools::execute(col, c)?)
        }
        ManageCommand::Notetypes(c) => {
            ManageResponse::Notetypes(crate::notetypes::execute(col, c)?)
        }
        ManageCommand::Stats(c) => ManageResponse::Stats(crate::stats::execute(col, c)?),
        ManageCommand::Transfer(c) => ManageResponse::Transfer(crate::transfer::execute(col, c)?),
        ManageCommand::Maintenance(c) => {
            ManageResponse::Maintenance(crate::maintenance::execute(col, c)?)
        }
        ManageCommand::Connect(c) => ManageResponse::Connect(crate::connect::execute(col, c)?),
    })
}
