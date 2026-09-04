use crate::game::Outcome;
use crate::types::Seat;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RulesError {
    #[error("the game is over")]
    GameOver { outcome: Outcome },
    #[error("{seat} does not need to act right now")]
    NotYourTurnToAct { seat: Seat },
    #[error("illegal action: {reason}")]
    IllegalAction { reason: String },
    #[error("unsupported in this build: {what}")]
    Unsupported { what: String },
    #[error("cannot set up game: {reason}")]
    Setup { reason: String },
}

impl RulesError {
    pub fn illegal(reason: impl Into<String>) -> RulesError {
        RulesError::IllegalAction { reason: reason.into() }
    }

    pub fn setup(reason: impl Into<String>) -> RulesError {
        RulesError::Setup { reason: reason.into() }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self, RulesError::NotYourTurnToAct { .. })
    }
}
