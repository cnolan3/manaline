//! London mulligan as two actions (§3.2): decided in turn order from the
//! starting player, one seat at a time.

use crate::event::Event;
use crate::format::MulliganRule;
use crate::game::{Game, PendingChoice};
use crate::types::{ObjectId, Seat, Zone};

impl Game {
    pub(crate) fn mulligan(&mut self, seat: Seat, keep: bool) {
        let i = seat.index();
        if keep {
            let taken = self.players[i].mulligans;
            let MulliganRule::London { free_first } = self.format.mulligan;
            let bottom = if free_first { taken.saturating_sub(1) } else { taken };
            let bottom = bottom.min(self.players[i].hand.len() as u8);
            self.emit(Event::HandKept { seat, size: self.players[i].hand.len() as u8 });
            if bottom > 0 {
                self.pending = Some(PendingChoice::BottomCards { seat, count: bottom });
            } else {
                self.advance_mulligan(seat);
            }
        } else {
            let hand = std::mem::take(&mut self.players[i].hand);
            for id in hand {
                self.objects[id].zone = Zone::Library;
                self.players[i].library.push(id);
            }
            self.players[i].mulligans += 1;
            self.shuffle_library(seat);
            self.draw(seat, self.format.starting_hand as usize);
            let to = self.format.starting_hand.saturating_sub(self.players[i].mulligans);
            self.emit(Event::MulliganTaken { seat, to });
            // The same seat decides again.
        }
    }

    pub(crate) fn bottom_cards(&mut self, seat: Seat, objects: &[ObjectId]) {
        let i = seat.index();
        for &id in objects {
            self.players[i].hand.retain(|&h| h != id);
            self.objects[id].zone = Zone::Library;
            self.players[i].library.insert(0, id);
        }
        self.emit(Event::Bottomed { seat, count: objects.len() as u8 });
        self.advance_mulligan(seat);
    }

    /// Hand the mulligan decision to the next seat in turn order, or start the game.
    pub(crate) fn advance_mulligan(&mut self, seat: Seat) {
        let pos = self.seating.iter().position(|&s| s == seat).unwrap_or(0);
        let next = self.seating[pos + 1..]
            .iter()
            .copied()
            .find(|&s| !self.is_eliminated(s));
        self.pending = next.map(|s| PendingChoice::Mulligan { seat: s });
    }
}
