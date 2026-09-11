//! The action vocabulary (§3.2). Deliberately small.

use crate::types::{Mana, ObjectId, Seat};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackTarget {
    Player(Seat),
    Planeswalker(ObjectId),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DamageTarget {
    Player(Seat),
    Object(ObjectId),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Object(ObjectId),
    Player(Seat),
}

/// How a cost is paid: which permanents to tap for their mana, and which mana
/// already in the pool to spend. Enumerated by the engine per §10's
/// "by colour combination" rule; the engine picks concrete permanents.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ManaPayment {
    #[serde(default)]
    pub tap: Vec<ObjectId>,
    #[serde(default)]
    pub from_pool: Vec<Mana>,
    /// Permanents sacrificed to pay a "Sacrifice a creature" cost.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sacrifice: Vec<ObjectId>,
    /// Cards discarded to pay a "Discard a card" cost.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discard: Vec<ObjectId>,
    /// The value announced for `{X}` in the cost; ignored for costs without one.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub x: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The `ability` index that means "equip" on an Equipment.
pub const EQUIP_ABILITY: u8 = u8::MAX;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    PassPriority,
    PlayLand {
        object: ObjectId,
    },
    CastSpell {
        object: ObjectId,
        #[serde(default)]
        targets: Vec<Target>,
        #[serde(default)]
        payment: ManaPayment,
    },
    ActivateAbility {
        object: ObjectId,
        ability: u8,
        #[serde(default)]
        targets: Vec<Target>,
        #[serde(default)]
        payment: ManaPayment,
    },
    DeclareAttackers {
        attackers: Vec<(ObjectId, AttackTarget)>,
    },
    /// `(blocker, attacker)` pairs.
    DeclareBlockers {
        blocks: Vec<(ObjectId, ObjectId)>,
    },
    /// Only offered when a real choice exists (§3.2). Validated by rule, not by list membership.
    AssignCombatDamage {
        attacker: ObjectId,
        assignments: Vec<(DamageTarget, i32)>,
    },
    /// Answering a `PendingChoice`.
    ChooseTargets {
        targets: Vec<Target>,
    },
    ChooseMode {
        mode: u8,
    },
    /// Cleanup-step hand size.
    Discard {
        objects: Vec<ObjectId>,
    },
    Mulligan {
        keep: bool,
    },
    /// London mulligan: after keeping, put N cards on the bottom.
    BottomCards {
        objects: Vec<ObjectId>,
    },
    CastCommander {
        object: ObjectId,
        #[serde(default)]
        targets: Vec<Target>,
        #[serde(default)]
        payment: ManaPayment,
    },
    CommanderToCommandZone {
        object: ObjectId,
    },
    Concede,
}

impl Action {
    /// Actions whose legal space is a division of a resource among recipients
    /// (§3's carve-out): combat damage among blockers, and — in the same spirit —
    /// a player's creatures among attack targets or among attackers to block.
    /// These are validated by rule rather than by list membership; the entries
    /// `legal_actions` enumerates for them are suggestions.
    pub fn is_division(&self) -> bool {
        matches!(
            self,
            Action::AssignCombatDamage { .. } | Action::DeclareAttackers { .. } | Action::DeclareBlockers { .. }
        )
    }

    /// Order-insensitive form used for list-membership checks: the vectors
    /// inside set-like actions are sorted so `[a, b]` and `[b, a]` compare equal.
    pub fn canonical(&self) -> Action {
        let mut a = self.clone();
        match &mut a {
            Action::CastSpell { payment, .. } | Action::ActivateAbility { payment, .. } | Action::CastCommander { payment, .. } => {
                payment.tap.sort();
                payment.from_pool.sort();
                payment.sacrifice.sort();
                payment.discard.sort();
            }
            Action::ChooseTargets { targets } => targets.sort(),
            Action::DeclareAttackers { attackers } => attackers.sort(),
            Action::DeclareBlockers { blocks } => blocks.sort(),
            Action::AssignCombatDamage { assignments, .. } => assignments.sort(),
            Action::Discard { objects } | Action::BottomCards { objects } => objects.sort(),
            _ => {}
        }
        a
    }

    pub fn is_pass(&self) -> bool {
        matches!(self, Action::PassPriority)
    }

    pub fn payment(&self) -> Option<&ManaPayment> {
        match self {
            Action::CastSpell { payment, .. } | Action::ActivateAbility { payment, .. } | Action::CastCommander { payment, .. } => {
                Some(payment)
            }
            _ => None,
        }
    }

    /// The same cast or activation, ignoring which permanents pay the mana.
    pub fn same_except_mana(&self, other: &Action) -> bool {
        match (self, other) {
            (
                Action::CastSpell {
                    object: a, targets: ta, ..
                },
                Action::CastSpell {
                    object: b, targets: tb, ..
                },
            ) => a == b && ta == tb,
            (
                Action::ActivateAbility {
                    object: a,
                    ability: ia,
                    targets: ta,
                    payment: pa,
                },
                Action::ActivateAbility {
                    object: b,
                    ability: ib,
                    targets: tb,
                    payment: pb,
                },
            ) => a == b && ia == ib && ta == tb && pa.sacrifice == pb.sacrifice && pa.discard == pb.discard,
            _ => false,
        }
    }

    /// The mana this action must pay, if it is a cast or activation.
    pub fn mana_cost_in(&self, game: &crate::game::Game) -> Option<crate::types::ManaCost> {
        match self {
            Action::CastSpell { object, .. } => game.objects.get(*object).map(|o| game.cast_cost(o.controller, *object)),
            Action::ActivateAbility { object, ability, .. } => {
                game.objects.get(*object)?;
                let def = game.card_def(*object);
                if *ability == EQUIP_ABILITY {
                    return def.ir.equip.clone();
                }
                let a = def.ir.activated.get(*ability as usize)?;
                let mut total = crate::types::ManaCost::default();
                for c in &a.cost {
                    if let cardir::Cost::Mana(m) = c {
                        total.generic += m.generic;
                        total.pips.extend(m.pips.iter().copied());
                    }
                }
                total.pips.sort();
                Some(total)
            }
            _ => None,
        }
    }
}
