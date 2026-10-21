//! Texas hold'em, as the poker tables play it (`poker`): a deck, the best
//! hand of seven cards, a hand's betting (blinds, the four streets, all-ins
//! and side pots, the showdown), the other players' choices, and the odds of
//! each kind of hand by the river that the table's movie shows the player.

/// A card: rank 2..=14 (ace high) and suit 0..4, as `rank * 4 + suit`
/// counted from the twos.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Card(pub u8);

impl Card {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(rank: u8, suit: u8) -> Self {
        Card((rank - 2) * 4 + suit)
    }

    pub fn rank(self) -> u8 {
        self.0 / 4 + 2
    }

    pub fn suit(self) -> u8 {
        self.0 % 4
    }
}

/// A small generator of its own (xorshift*), seeded by the game: shuffles
/// and the players' whims.
#[derive(Clone, Debug)]
pub struct Dealer(u64);

impl Dealer {
    pub fn new(seed: u64) -> Self {
        Dealer(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0..1.
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// The 52 cards shuffled.
    pub fn deck(&mut self) -> Vec<Card> {
        let mut deck: Vec<Card> = (0..52).map(Card).collect();
        for i in (1..deck.len()).rev() {
            let j = self.below(i + 1);
            deck.swap(i, j);
        }
        deck
    }
}

/// The kinds of hand, weakest first (as the movie's odds box lists them).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Category {
    HighCard,
    Pair,
    TwoPair,
    ThreeOfAKind,
    Straight,
    Flush,
    FullHouse,
    FourOfAKind,
    StraightFlush,
    RoyalFlush,
}

/// A hand's worth: its kind, then the ranks that break ties, in order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct HandValue {
    pub category: Category,
    pub ranks: [u8; 5],
}

/// The highest card of a straight among the ranks present (`present[r]` for
/// rank r, the ace also low).
fn straight_high(present: &[bool; 15]) -> Option<u8> {
    (5..=14u8).rev().find(|&high| (high - 4..=high).all(|r| present[if r == 1 { 14 } else { r as usize }]))
}

/// The best five of up to seven cards.
pub fn evaluate(cards: &[Card]) -> HandValue {
    let mut counts = [0u8; 15];
    let mut suits = [0u8; 4];
    for c in cards {
        counts[c.rank() as usize] += 1;
        suits[c.suit() as usize] += 1;
    }
    let present = |filter: &dyn Fn(Card) -> bool| {
        let mut p = [false; 15];
        for c in cards.iter().filter(|c| filter(**c)) {
            p[c.rank() as usize] = true;
        }
        p
    };
    let value = |category, ranks: &[u8]| {
        let mut r = [0u8; 5];
        r[..ranks.len().min(5)].copy_from_slice(&ranks[..ranks.len().min(5)]);
        HandValue { category, ranks: r }
    };
    // Flushes, straight or not.
    if let Some(suit) = (0..4u8).find(|&s| suits[s as usize] >= 5) {
        let in_suit = present(&|c: Card| c.suit() == suit);
        if let Some(high) = straight_high(&in_suit) {
            return value(if high == 14 { Category::RoyalFlush } else { Category::StraightFlush }, &[high]);
        }
        let mut ranks: Vec<u8> = cards.iter().filter(|c| c.suit() == suit).map(|c| c.rank()).collect();
        ranks.sort_unstable_by(|a, b| b.cmp(a));
        return value(Category::Flush, &ranks[..5]);
    }
    // By how many of a rank, then the rank, highest first.
    let mut groups: Vec<(u8, u8)> = (2..=14u8).rev().filter(|&r| counts[r as usize] > 0).map(|r| (counts[r as usize], r)).collect();
    groups.sort_by(|a, b| b.cmp(a));
    let kickers = |used: &[u8], n: usize| -> Vec<u8> { (2..=14u8).rev().filter(|r| counts[*r as usize] > 0 && !used.contains(r)).take(n).collect() };
    match groups[0].0 {
        4 => {
            let quad = groups[0].1;
            return value(Category::FourOfAKind, &[&[quad][..], &kickers(&[quad], 1)].concat());
        }
        3 => {
            // (A second three of a kind makes the pair of a full house.)
            if let Some(&(_, pair)) = groups.iter().skip(1).find(|g| g.0 >= 2) {
                return value(Category::FullHouse, &[groups[0].1, pair]);
            }
        }
        _ => {}
    }
    if let Some(high) = straight_high(&present(&|_| true)) {
        return value(Category::Straight, &[high]);
    }
    let top = groups[0].1;
    match groups[0].0 {
        3 => value(Category::ThreeOfAKind, &[&[top][..], &kickers(&[top], 2)].concat()),
        2 if groups.get(1).is_some_and(|g| g.0 == 2) => {
            let second = groups[1].1;
            value(Category::TwoPair, &[&[top, second][..], &kickers(&[top, second], 1)].concat())
        }
        2 => value(Category::Pair, &[&[top][..], &kickers(&[top], 3)].concat()),
        _ => value(Category::HighCard, &kickers(&[], 5)),
    }
}

/// A betting round's stage.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Street {
    PreFlop,
    Flop,
    Turn,
    River,
    /// The hand is over (shown down, or all but one folded).
    Over,
}

/// What a player does when it's their turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Fold,
    /// Nothing to call (else it's a call).
    Check,
    Call,
    /// To this much in front of them on this street (at least the least
    /// raise, unless it's all they have).
    Raise(u32),
    AllIn,
}

/// A player at the table.
#[derive(Clone, Debug, Default)]
pub struct Seat {
    pub chips: u32,
    pub hole: Option<[Card; 2]>,
    pub folded: bool,
    /// In front of them on this street, and in the pot this hand.
    pub bet: u32,
    pub put_in: u32,
    /// Has acted since the last raise.
    acted: bool,
    /// Out of the game (no chips left to play a hand with).
    pub out: bool,
}

impl Seat {
    fn live(&self) -> bool {
        self.hole.is_some() && !self.folded
    }
}

/// A hand's end: what each seat won, and each seat shown down's hand.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub won: Vec<u32>,
    pub shown: Vec<Option<HandValue>>,
}

/// The table: its players, the button, the blinds and the hand being
/// played.
#[derive(Clone, Debug)]
pub struct Table {
    pub seats: Vec<Seat>,
    pub button: usize,
    pub small_blind: u32,
    pub big_blind: u32,
    pub board: Vec<Card>,
    pub street: Street,
    deck: Vec<Card>,
    /// Whose turn it is.
    pub to_act: Option<usize>,
    /// The bet to match on this street, and the least raise over it.
    pub current_bet: u32,
    pub min_raise: u32,
    pub outcome: Option<Outcome>,
}

impl Table {
    pub fn new(chips: &[u32], small_blind: u32, big_blind: u32) -> Self {
        Table {
            seats: chips.iter().map(|&c| Seat { chips: c, out: c == 0, ..Default::default() }).collect(),
            button: 0,
            small_blind,
            big_blind,
            board: Vec::new(),
            street: Street::Over,
            deck: Vec::new(),
            to_act: None,
            current_bet: 0,
            min_raise: big_blind,
            outcome: None,
        }
    }

    /// The pot: all that's been put in this hand.
    pub fn pot(&self) -> u32 {
        self.seats.iter().map(|s| s.put_in).sum()
    }

    /// The seats still with chips to play.
    pub fn players(&self) -> usize {
        self.seats.iter().filter(|s| !s.out).count()
    }

    /// The next seat after `from` still owing this street an action: in
    /// the hand, with chips, and yet to act since the last raise or short of
    /// the bet.
    fn next_to_act(&self, from: usize) -> Option<usize> {
        let n = self.seats.len();
        (1..=n).map(|i| (from + i) % n).find(|&i| {
            let s = &self.seats[i];
            s.live() && s.chips > 0 && (!s.acted || s.bet < self.current_bet)
        })
    }

    fn next_in_game(&self, from: usize) -> usize {
        let n = self.seats.len();
        (1..=n).map(|i| (from + i) % n).find(|&i| !self.seats[i].out).unwrap_or(from)
    }

    fn put(&mut self, seat: usize, amount: u32) {
        let s = &mut self.seats[seat];
        let amount = amount.min(s.chips);
        s.chips -= amount;
        s.bet += amount;
        s.put_in += amount;
    }

    /// The cards still in the deck, in the order they would be dealt.
    pub fn upcoming(&self) -> impl Iterator<Item = Card> + '_ {
        self.deck.iter().rev().copied()
    }

    /// Deals a new hand: the button moves on, the blinds go in, two cards
    /// each, and the first to act after the big blind is to act.
    pub fn deal(&mut self, dealer: &mut Dealer) {
        for s in &mut self.seats {
            s.out = s.chips == 0;
            s.hole = None;
            s.folded = false;
            s.bet = 0;
            s.put_in = 0;
            s.acted = false;
        }
        self.board.clear();
        self.outcome = None;
        if self.players() < 2 {
            self.street = Street::Over;
            self.to_act = None;
            return;
        }
        self.deck = dealer.deck();
        self.button = self.next_in_game(self.button);
        for i in 0..self.seats.len() {
            if !self.seats[i].out {
                let (a, b) = (self.deck.pop().unwrap(), self.deck.pop().unwrap());
                self.seats[i].hole = Some([a, b]);
            }
        }
        // (Two players: the button is the small blind and acts first.)
        let heads_up = self.players() == 2;
        let small = if heads_up { self.button } else { self.next_in_game(self.button) };
        let big = self.next_in_game(small);
        self.put(small, self.small_blind);
        self.put(big, self.big_blind);
        self.street = Street::PreFlop;
        self.current_bet = self.seats[small].bet.max(self.seats[big].bet);
        self.min_raise = self.big_blind;
        self.to_act = self.next_to_act(big);
        self.settle_if_done();
    }

    /// What the seat to act may do: the amount to call, and the least and
    /// most it may raise to.
    pub fn options(&self, seat: usize) -> (u32, u32, u32) {
        let s = &self.seats[seat];
        let call = self.current_bet.saturating_sub(s.bet).min(s.chips);
        let most = s.bet + s.chips;
        let least = (self.current_bet + self.min_raise).min(most);
        (call, least, most)
    }

    /// The seat to act does what it does (anything not possible is taken as
    /// the nearest that is: a check with something to call folds; a raise
    /// below the least is the least).
    pub fn act(&mut self, seat: usize, action: Action) {
        if self.to_act != Some(seat) {
            return;
        }
        let (call, least, most) = self.options(seat);
        match action {
            Action::Fold => self.seats[seat].folded = true,
            Action::Check if call > 0 => self.seats[seat].folded = true,
            Action::Check => {}
            Action::Call => self.put(seat, call),
            Action::AllIn => self.raise_to(seat, most),
            Action::Raise(to) => self.raise_to(seat, to.clamp(least, most)),
        }
        self.seats[seat].acted = true;
        self.to_act = self.next_to_act(seat);
        self.settle_if_done();
    }

    fn raise_to(&mut self, seat: usize, to: u32) {
        let more = to.saturating_sub(self.seats[seat].bet);
        self.put(seat, more);
        let now = self.seats[seat].bet;
        if now > self.current_bet {
            // (A full raise reopens the betting; a short all-in only sets
            // what's to call.)
            if now - self.current_bet >= self.min_raise {
                self.min_raise = now - self.current_bet;
                for (i, s) in self.seats.iter_mut().enumerate() {
                    if i != seat {
                        s.acted = false;
                    }
                }
            }
            self.current_bet = now;
        }
    }

    /// Ends the street when everyone still betting has acted and matched,
    /// and the hand when one is left or it's shown down.
    fn settle_if_done(&mut self) {
        loop {
            if self.street == Street::Over {
                return;
            }
            let live: Vec<usize> = (0..self.seats.len()).filter(|&i| self.seats[i].live()).collect();
            if live.len() == 1 {
                self.finish(&live);
                return;
            }
            let betting: Vec<usize> = live.iter().copied().filter(|&i| self.seats[i].chips > 0).collect();
            let matched = betting.iter().all(|&i| self.seats[i].bet == self.current_bet || self.seats[i].chips == 0);
            let all_acted = betting.iter().all(|&i| self.seats[i].acted);
            // (One left with chips facing nothing more: no betting left.)
            let alone = betting.len() <= 1 && betting.iter().all(|&i| self.seats[i].bet >= self.current_bet);
            if !(matched && (all_acted || alone)) {
                return;
            }
            // On to the next street.
            for s in &mut self.seats {
                s.bet = 0;
                s.acted = false;
            }
            self.current_bet = 0;
            self.min_raise = self.big_blind;
            self.street = match self.street {
                Street::PreFlop => {
                    self.board.extend([self.deck.pop().unwrap(), self.deck.pop().unwrap(), self.deck.pop().unwrap()]);
                    Street::Flop
                }
                Street::Flop => {
                    self.board.push(self.deck.pop().unwrap());
                    Street::Turn
                }
                Street::Turn => {
                    self.board.push(self.deck.pop().unwrap());
                    Street::River
                }
                _ => {
                    self.finish(&live);
                    return;
                }
            };
            self.to_act = self.next_to_act(self.button);
            if betting.len() > 1 {
                return;
            }
            // (Everyone else all in: the rest of the board comes.)
        }
    }

    /// The pots to their winners: each pot (side pots by how much each
    /// all-in could match) to the best hand among those in it, split evenly
    /// (the odd chip to the first after the button).
    fn finish(&mut self, live: &[usize]) {
        let n = self.seats.len();
        let mut won = vec![0u32; n];
        let mut shown = vec![None; n];
        if live.len() == 1 {
            won[live[0]] = self.pot();
        } else {
            for &i in live {
                let hole = self.seats[i].hole.unwrap();
                shown[i] = Some(evaluate(&[&hole[..], &self.board].concat()));
            }
            let mut levels: Vec<u32> = live.iter().map(|&i| self.seats[i].put_in).collect();
            levels.sort_unstable();
            levels.dedup();
            let mut below = 0;
            for level in levels {
                let pot: u32 = self.seats.iter().map(|s| s.put_in.min(level).saturating_sub(below)).sum();
                let contenders: Vec<usize> = live.iter().copied().filter(|&i| self.seats[i].put_in >= level).collect();
                let best = contenders.iter().filter_map(|&i| shown[i]).max();
                let winners: Vec<usize> = contenders.iter().copied().filter(|&i| shown[i] == best).collect();
                let share = pot / winners.len() as u32;
                for &w in &winners {
                    won[w] += share;
                }
                let odd = pot - share * winners.len() as u32;
                if odd > 0 {
                    let first = (1..=n).map(|k| (self.button + k) % n).find(|i| winners.contains(i)).unwrap();
                    won[first] += odd;
                }
                below = level;
            }
            // (What nobody still in could match goes back: a folded
            // player's overbet beyond every live stake.)
            let top = live.iter().map(|&i| self.seats[i].put_in).max().unwrap_or(0);
            for (i, s) in self.seats.iter().enumerate() {
                if s.put_in > top {
                    won[i] += s.put_in - top;
                }
            }
        }
        for (s, w) in self.seats.iter_mut().zip(&won) {
            s.chips += w;
        }
        self.street = Street::Over;
        self.to_act = None;
        self.outcome = Some(Outcome { won, shown });
    }
}

/// How likely `hole` with `board` is to win against `opponents` random
/// hands by the river (ties count half), from `trials` deals.
pub fn equity(hole: [Card; 2], board: &[Card], opponents: usize, trials: usize, dealer: &mut Dealer) -> f32 {
    let known: Vec<Card> = [&hole[..], board].concat();
    let rest: Vec<Card> = (0..52).map(Card).filter(|c| !known.contains(c)).collect();
    let mut score = 0.0;
    for _ in 0..trials {
        let mut deck = rest.clone();
        let mut draw = || deck.swap_remove(dealer.below(deck.len()));
        let mut full = board.to_vec();
        while full.len() < 5 {
            full.push(draw());
        }
        let mine = evaluate(&[&hole[..], &full].concat());
        let mut best_other = None;
        for _ in 0..opponents {
            let theirs = evaluate(&[&[draw(), draw()][..], &full].concat());
            best_other = best_other.max(Some(theirs));
        }
        score += match best_other.map(|o| mine.cmp(&o)) {
            None | Some(std::cmp::Ordering::Greater) => 1.0,
            Some(std::cmp::Ordering::Equal) => 0.5,
            Some(std::cmp::Ordering::Less) => 0.0,
        };
    }
    score / trials.max(1) as f32
}

/// The chances of each kind of hand by the river (`Category`'s order) from
/// `hole` and the board so far, from `trials` deals (all certain once the
/// board is out): the table's help screen's odds.
pub fn odds(hole: [Card; 2], board: &[Card], trials: usize, dealer: &mut Dealer) -> [f32; 10] {
    let known: Vec<Card> = [&hole[..], board].concat();
    let rest: Vec<Card> = (0..52).map(Card).filter(|c| !known.contains(c)).collect();
    let mut counts = [0u32; 10];
    let trials = if board.len() >= 5 { 1 } else { trials.max(1) };
    for _ in 0..trials {
        let mut deck = rest.clone();
        let mut full = known.clone();
        while full.len() < 7 {
            full.push(deck.swap_remove(dealer.below(deck.len())));
        }
        counts[evaluate(&full).category as usize] += 1;
    }
    counts.map(|c| c as f32 / trials as f32)
}

/// How a computer player plays: how much it needs to stay in, how readily
/// it raises, and how often it bluffs.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// Added to the pot odds it needs to call (tight players need more).
    pub caution: f32,
    /// The equity over which it raises.
    pub raise_at: f32,
    pub bluff: f32,
}

impl Style {
    pub const STEADY: Style = Style { caution: 0.05, raise_at: 0.62, bluff: 0.05 };
    pub const LOOSE: Style = Style { caution: -0.05, raise_at: 0.55, bluff: 0.12 };
    #[cfg_attr(not(test), allow(dead_code))]
    pub const TIGHT: Style = Style { caution: 0.12, raise_at: 0.7, bluff: 0.02 };
}

/// A computer player's choice: its chances against the others still in
/// (by simulation) against what calling costs, raising a share of the pot
/// with a strong hand (or now and then a bluff).
pub fn decide(table: &Table, seat: usize, style: Style, dealer: &mut Dealer) -> Action {
    let Some(hole) = table.seats[seat].hole else { return Action::Fold };
    let opponents = table.seats.iter().enumerate().filter(|(i, s)| *i != seat && s.live()).count().max(1);
    let win = equity(hole, &table.board, opponents, 160, dealer);
    let (call, least, most) = table.options(seat);
    let pot = table.pot();
    let needed = if call == 0 { 0.0 } else { call as f32 / (pot + call) as f32 } + style.caution;
    let bluffing = dealer.unit() < style.bluff;
    if (win > style.raise_at || bluffing) && least < most && least > table.current_bet {
        // (Between half the pot and the whole, more the better the hand.)
        let share = 0.5 + (win - style.raise_at).max(0.0) * 1.5;
        let to = table.current_bet + ((pot as f32 * share) as u32).max(least - table.current_bet);
        return if to >= most { Action::AllIn } else { Action::Raise(to) };
    }
    if call == 0 {
        return Action::Check;
    }
    if win >= needed || (call <= table.big_blind && win > 0.25) {
        Action::Call
    } else {
        Action::Fold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cards(s: &str) -> Vec<Card> {
        s.split_whitespace()
            .map(|c| {
                let (r, su) = c.split_at(c.len() - 1);
                let rank = match r {
                    "A" => 14,
                    "K" => 13,
                    "Q" => 12,
                    "J" => 11,
                    "T" => 10,
                    n => n.parse().unwrap(),
                };
                let suit = "cdhs".find(su).unwrap() as u8;
                Card::new(rank, suit)
            })
            .collect()
    }

    fn kind(s: &str) -> Category {
        evaluate(&cards(s)).category
    }

    /// Each kind of hand from seven cards, the best five taken.
    #[test]
    fn seven_cards_make_the_best_hand() {
        assert_eq!(kind("As Ks Qs Js Ts 2d 3c"), Category::RoyalFlush);
        assert_eq!(kind("9s Ks Qs Js Ts 2d 3c"), Category::StraightFlush);
        assert_eq!(kind("As 2s 3s 4s 5s Kd Kc"), Category::StraightFlush);
        assert_eq!(kind("9s 9d 9h 9c Ts 2d 3c"), Category::FourOfAKind);
        assert_eq!(kind("9s 9d 9h Tc Ts 2d 3c"), Category::FullHouse);
        assert_eq!(kind("9s 9d 9h Tc Ts Td 3c"), Category::FullHouse);
        assert_eq!(kind("2s 7s 9s Js Ks Kd Kc"), Category::Flush);
        assert_eq!(kind("Ad 2c 3h 4s 5d Kd Kc"), Category::Straight);
        assert_eq!(kind("9s 9d 9h 2c 4s 6d Kc"), Category::ThreeOfAKind);
        assert_eq!(kind("9s 9d 4h 4c 2s 6d Kc"), Category::TwoPair);
        assert_eq!(kind("9s 9d 4h 7c 2s 6d Kc"), Category::Pair);
        assert_eq!(kind("9s Td 4h 7c 2s 6d Kc"), Category::HighCard);
    }

    /// Hands of a kind compare by their ranks, kickers last.
    #[test]
    fn ties_break_on_ranks_then_kickers() {
        let v = |s: &str| evaluate(&cards(s));
        assert!(v("9s 9d Ah 7c 2s 6d 3c") > v("9h 9c Kh 7d 2c 6h 3d"));
        assert!(v("Ts Td 2h 2c 9s") > v("9h 9c 8h 8d Ac"));
        assert!(v("Ts Td 2h 2c 9s") < v("9h 9c Kh Kd Ac"));
        assert!(v("6s 5d 4h 3c 2s") > v("5h 4c 3h 2d Ac"));
        assert_eq!(v("Ks Kd 9h 7c 2s"), v("Kh Kc 9d 7s 2d"));
        // (Three pairs: the best two, the third's rank a kicker.)
        assert_eq!(v("9s 9d 4h 4c 6s 6d 2c").ranks[..3], [9, 6, 4]);
    }

    /// Everyone checking down: the board comes out street by street and the
    /// pot goes to the best hand.
    #[test]
    fn a_hand_checked_down_goes_to_the_best_hand() {
        let mut t = Table::new(&[1000, 1000, 1000], 5, 10);
        let mut d = Dealer::new(7);
        t.deal(&mut d);
        assert_eq!(t.street, Street::PreFlop);
        assert_eq!(t.pot(), 15);
        let mut guard = 0;
        while let Some(seat) = t.to_act {
            let (call, ..) = t.options(seat);
            t.act(seat, if call > 0 { Action::Call } else { Action::Check });
            guard += 1;
            assert!(guard < 50);
        }
        assert_eq!(t.street, Street::Over);
        assert_eq!(t.board.len(), 5);
        let o = t.outcome.clone().unwrap();
        assert_eq!(o.won.iter().sum::<u32>(), 30);
        assert_eq!(t.seats.iter().map(|s| s.chips).sum::<u32>(), 3000);
    }

    /// All but one folding hands the pot over without a showdown.
    #[test]
    fn folds_leave_the_pot_to_the_last_one_in() {
        let mut t = Table::new(&[1000, 1000, 1000], 5, 10);
        t.deal(&mut Dealer::new(3));
        let first = t.to_act.unwrap();
        t.act(first, Action::Raise(40));
        let second = t.to_act.unwrap();
        t.act(second, Action::Fold);
        let third = t.to_act.unwrap();
        t.act(third, Action::Fold);
        assert_eq!(t.street, Street::Over);
        let o = t.outcome.unwrap();
        assert_eq!(o.won[first], 40 + 5 + 10);
        assert!(o.shown.iter().all(Option::is_none));
    }

    /// A short all-in makes a side pot the bigger stacks play for alone.
    #[test]
    fn a_short_all_in_wins_only_the_pot_it_could_match() {
        let mut t = Table::new(&[100, 1000, 1000], 5, 10);
        let mut d = Dealer::new(11);
        t.deal(&mut d);
        // Everyone in: the short stack all in, the others calling and then
        // checking it down.
        while let Some(seat) = t.to_act {
            let (call, ..) = t.options(seat);
            let action = if t.seats[seat].chips <= 100 && t.street == Street::PreFlop {
                Action::AllIn
            } else if call > 0 {
                Action::Call
            } else {
                Action::Check
            };
            t.act(seat, action);
        }
        let o = t.outcome.clone().unwrap();
        let short = 0;
        assert!(o.won[short] <= 300, "{:?}", o.won);
        assert_eq!(t.seats.iter().map(|s| s.chips).sum::<u32>(), 2100);
    }

    /// Two players: the button posts the small blind and acts first.
    #[test]
    fn heads_up_the_button_is_the_small_blind() {
        let mut t = Table::new(&[500, 500], 5, 10);
        t.deal(&mut Dealer::new(5));
        assert_eq!(t.seats[t.button].put_in, 5);
        assert_eq!(t.to_act, Some(t.button));
    }

    /// The odds of every kind by the river add up, and are certain once the
    /// board is out.
    #[test]
    fn hand_odds_add_up() {
        let mut d = Dealer::new(9);
        let hole = cards("As Ad");
        let o = odds([hole[0], hole[1]], &[], 400, &mut d);
        assert!((o.iter().sum::<f32>() - 1.0).abs() < 1e-3);
        assert_eq!(o[Category::HighCard as usize], 0.0);
        let board = cards("Ah Ac 2d 7s 9c");
        let o = odds([hole[0], hole[1]], &board, 400, &mut d);
        assert_eq!(o[Category::FourOfAKind as usize], 1.0);
    }

    /// The computer players fold rubbish facing a big bet and raise the
    /// nuts.
    #[test]
    fn the_computer_players_play_their_cards() {
        let mut d = Dealer::new(21);
        let mut t = Table::new(&[1000, 1000], 5, 10);
        t.deal(&mut d);
        let seat = t.to_act.unwrap();
        let other = 1 - seat;
        t.seats[seat].hole = Some([Card::new(14, 0), Card::new(14, 1)]);
        assert!(matches!(decide(&t, seat, Style::STEADY, &mut d), Action::Raise(_) | Action::AllIn | Action::Call));
        t.act(seat, Action::Raise(400));
        t.seats[other].hole = Some([Card::new(2, 0), Card::new(7, 1)]);
        assert_eq!(decide(&t, other, Style::TIGHT, &mut d), Action::Fold);
    }
}
