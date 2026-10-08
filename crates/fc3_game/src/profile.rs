//! Frame profiling (`--features profile`, which turns on Bevy's per-system
//! spans): a tracing layer that sums the time spent in each system and
//! schedule and logs the largest every ten seconds, then the longest single
//! runs; and for any frame over 30 ms, what ran longer than 4 ms in it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bevy::log::tracing::span::{Attributes, Id};
use bevy::log::tracing::{field, Subscriber};
use bevy::log::tracing_subscriber::layer::Context;
use bevy::log::tracing_subscriber::registry::LookupSpan;
use bevy::log::tracing_subscriber::Layer;
use bevy::log::BoxedLayer;
use bevy::prelude::*;

/// Time, runs and longest run per span (`kind:name`) since the last report.
static TOTALS: Mutex<Option<(Instant, HashMap<String, (Duration, u32, Duration)>)>> = Mutex::new(None);
/// Spans over `SLOW_SPAN` since the last frame.
static SLOW: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());
const SLOW_SPAN: Duration = Duration::from_millis(4);
const SLOW_FRAME: Duration = Duration::from_millis(30);

struct Name(String);
struct Entered(Instant);

#[derive(Default)]
struct NameVisitor(Option<String>);

impl field::Visit for NameVisitor {
    fn record_str(&mut self, f: &field::Field, value: &str) {
        if f.name() == "name" || f.name() == "system" {
            self.0 = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, f: &field::Field, value: &dyn std::fmt::Debug) {
        if f.name() == "name" || f.name() == "system" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

struct Totals;

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Totals {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut v = NameVisitor::default();
        attrs.record(&mut v);
        let kind = attrs.metadata().name();
        let name = match v.0 {
            Some(n) => format!("{kind}:{n}"),
            None => kind.to_string(),
        };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Name(name));
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            ext.remove::<Entered>();
            ext.insert(Entered(Instant::now()));
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        let Some(Entered(t)) = ext.remove::<Entered>() else { return };
        let Some(Name(name)) = ext.get_mut::<Name>() else { return };
        let mut totals = TOTALS.lock().unwrap();
        let (_, map) = totals.get_or_insert_with(|| (Instant::now(), HashMap::new()));
        let e = map.entry(name.clone()).or_default();
        let d = t.elapsed();
        e.0 += d;
        e.1 += 1;
        e.2 = e.2.max(d);
        if d > SLOW_SPAN {
            SLOW.lock().unwrap().push((name.clone(), d));
        }
    }
}

/// The layer, for `LogPlugin::custom_layer`.
pub fn layer(_: &mut App) -> Option<BoxedLayer> {
    Some(Box::new(Totals))
}

/// Logs the spans that took the most time per frame over the last ten
/// seconds, and what a slow frame was spent on.
pub fn report(mut frames: Local<u32>, time: Res<Time<Real>>) {
    *frames += 1;
    let slow = std::mem::take(&mut *SLOW.lock().unwrap());
    if time.delta() > SLOW_FRAME && time.elapsed_secs() > 15.0 {
        let mut spans: Vec<String> = slow
            .iter()
            .filter(|(n, _)| !n.starts_with("schedule:") && !n.starts_with("sub app") && n != "update" && n != "main app")
            .map(|(n, d)| format!("{:.1} {n}", d.as_secs_f64() * 1000.0))
            .collect();
        spans.sort();
        info!("slow frame {:.1} ms: {}", time.delta_secs() * 1000.0, spans.join(" | "));
    }
    let mut totals = TOTALS.lock().unwrap();
    let Some((since, map)) = totals.as_mut() else { return };
    if since.elapsed() < Duration::from_secs(10) {
        return;
    }
    let mut rows: Vec<_> = map.drain().collect();
    rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let n = (*frames).max(1) as f64;
    let mut out = format!("profile over {n} frames (ms per frame, runs per frame):");
    for (name, (d, runs, _)) in rows.iter().take(120) {
        out += &format!("\n  {:7.3} {:5.1} {name}", d.as_secs_f64() * 1000.0 / n, *runs as f64 / n);
    }
    rows.sort_by(|a, b| b.1 .2.cmp(&a.1 .2));
    out += "\nlongest single runs (ms):";
    for (name, (_, _, longest)) in rows.iter().take(40) {
        out += &format!("\n  {:7.2} {name}", longest.as_secs_f64() * 1000.0);
    }
    info!("{out}");
    *since = Instant::now();
    *frames = 0;
}
