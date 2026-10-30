use fn_ui::screens::athena_lobby::{MatchmakingView, MmHeader, MM_NS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step { CheckingForUpdates, WaitingForReady, ContactingGameService, ConnectingToMatchmakingService, FindingMatch, WaitingForServer, JoiningServer, LoadingContent, Done }

impl Step {
    fn hold(self) -> f32 {
        match self {
            Step::CheckingForUpdates => 0.4, Step::WaitingForReady => 0.8, Step::ContactingGameService => 0.6, Step::ConnectingToMatchmakingService => 0.8,
            Step::FindingMatch => 1.5, Step::WaitingForServer => 0.8, Step::JoiningServer => 0.8, Step::LoadingContent | Step::Done => 0.0,
        }
    }
    fn next(self) -> Step {
        match self {
            Step::CheckingForUpdates => Step::WaitingForReady, Step::WaitingForReady => Step::ContactingGameService, Step::ContactingGameService => Step::ConnectingToMatchmakingService,
            Step::ConnectingToMatchmakingService => Step::FindingMatch, Step::FindingMatch => Step::WaitingForServer, Step::WaitingForServer => Step::JoiningServer,
            Step::JoiningServer => Step::LoadingContent, Step::LoadingContent | Step::Done => Step::Done,
        }
    }
    fn key(self) -> &'static str {
        match self {
            Step::CheckingForUpdates => "Message.CheckingForUpdates", Step::WaitingForReady => "Message.WaitingForReady", Step::ContactingGameService => "Message.ContactingGameService",
            Step::ConnectingToMatchmakingService => "Message.ConnectingToMatchmakingService", Step::FindingMatch => "Message.FindingMatch", Step::WaitingForServer => "Message.WaitingForServer",
            Step::JoiningServer => "Message.JoiningServer", Step::LoadingContent | Step::Done => "Message.LoadingContent",
        }
    }
}

pub struct Matchmaking { step: Step, t: f32, pub local_ready: bool }

impl Matchmaking {
    pub fn start() -> Matchmaking { Matchmaking { step: Step::CheckingForUpdates, t: 0.0, local_ready: false } }
    pub fn step(&self) -> Step { self.step }
    pub fn tick(&mut self, dt: f32, load_fraction: f32) { self.tick_party(dt, load_fraction, false) }
    pub fn tick_party(&mut self, dt: f32, load_fraction: f32, hold_for_party: bool) { self.tick_queue(dt, load_fraction, hold_for_party, false) }
    pub fn tick_queue(&mut self, dt: f32, load_fraction: f32, hold_for_party: bool, hold_for_match: bool) {
        self.t += dt;
        if hold_for_party && self.step == Step::WaitingForReady { self.t = self.t.min(self.step.hold()); return; }
        if hold_for_match && self.step == Step::FindingMatch { self.t = self.t.min(self.step.hold()); return; }
        if self.step == Step::LoadingContent {
            if load_fraction >= 1.0 { self.step = Step::Done; }
            return;
        }
        while self.step != Step::Done && self.step != Step::LoadingContent && self.t >= self.step.hold() {
            self.t -= self.step.hold();
            self.step = self.step.next();
            if self.step == Step::WaitingForReady { self.local_ready = true; }
        }
    }
    pub fn done(&self) -> bool { self.step == Step::Done }
    pub fn can_cancel(&self) -> bool { !matches!(self.step, Step::WaitingForServer | Step::JoiningServer | Step::LoadingContent | Step::Done) }
    pub fn is_matchmaking(&self) -> bool { !matches!(self.step, Step::CheckingForUpdates | Step::WaitingForReady) }

    pub fn view(&self, locres: Option<&fn_ui::screens::athena_lobby::store::Locres>, load_fraction: f32) -> MatchmakingView {
        let raw = locres.and_then(|l| l.get(MM_NS, self.step.key())).unwrap_or("").to_owned();
        let na = locres.and_then(|l| l.get(MM_NS, "Message.ETANotAvailable")).unwrap_or("").to_owned();
        let message = match self.step {
            Step::FindingMatch => raw.replace("{0}", "1").replace("{1}", &na),
            Step::LoadingContent | Step::Done => raw.replace("{0}", &format!("{}", (load_fraction.clamp(0.0, 1.0) * 100.0).floor() as i32)),
            _ => raw,
        };
        MatchmakingView { header: MmHeader::Matchmaking, message, is_matchmaking: self.is_matchmaking(), cancel_enabled: self.can_cancel() }
    }
}
