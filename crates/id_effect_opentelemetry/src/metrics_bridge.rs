//! Bridges the `next` [`MetricRegistry`] to OpenTelemetry metrics.
//!
//! [`bridge_metrics`] attaches a [`MetricListener`] to a registry and maps every series (a
//! [`MetricKey`]: name, kind and tags) to an OpenTelemetry instrument named after the metric,
//! with the key's tags as attributes (fiber tags from `Effect::tag_metrics` are already merged
//! into the key):
//!
//! | `next` kind | OpenTelemetry instrument | how values flow |
//! |---|---|---|
//! | counter | `u64` observable counter | pulled: the callback reads the registry at collection |
//! | gauge | `i64` observable gauge | pulled: the callback reads the registry at collection |
//! | histogram | `f64` histogram with the same bucket boundaries | pushed on each sample |
//! | summary | `f64` histogram (SDK default buckets) | pushed on each sample |
//! | frequency | `u64` counter with a `value` attribute | pushed on each occurrence |
//!
//! Series that exist before the bridge is attached are replayed, so attaching late loses no
//! counter or gauge reading (pushed kinds see only the samples recorded after attaching).
//!
//! The bridge lives as long as the returned [`MetricsBridge`] (or a clone): dropping the last
//! handle detaches it (its listener and callbacks become no-ops). The starter
//! ([`install_otel_starter`](crate::install_otel_starter)) attaches one to
//! [`MetricRegistry::global`] and keeps it in its guard.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};

use id_effect::next::{
  MetricKey, MetricKind, MetricListener, MetricRegistry, MetricState, MetricUpdate,
};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

/// The attribute carrying the observed value of a frequency metric.
const FREQUENCY_VALUE: &str = "value";

fn attributes(key: &MetricKey) -> Vec<KeyValue> {
  key
    .tags()
    .iter()
    .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
    .collect()
}

/// The series of one pulled instrument, read by its callback.
type Series = Arc<RwLock<Vec<(MetricKey, Vec<KeyValue>)>>>;

/// A pushed series: its instrument and attributes.
enum Pushed {
  Histogram(Histogram<f64>, Vec<KeyValue>),
  Frequency(Counter<u64>, Vec<KeyValue>),
}

#[derive(Default)]
struct Instruments {
  /// Pulled instruments by (name, kind label).
  observed: HashMap<(String, &'static str), Series>,
  /// Histograms (histogram and summary kinds) by (name, kind label).
  histograms: HashMap<(String, &'static str), Histogram<f64>>,
  /// Frequency counters by name.
  frequencies: HashMap<String, Counter<u64>>,
}

struct Shared {
  registry: MetricRegistry,
  meter: Meter,
  instruments: Mutex<Instruments>,
  pushed: RwLock<HashMap<MetricKey, Pushed>>,
}

impl Shared {
  fn register(self: &Arc<Self>, key: &MetricKey) {
    let name = key.name().to_owned();
    let label = key.kind().label();
    let attrs = attributes(key);
    let mut ins = self
      .instruments
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    match key.kind() {
      MetricKind::Counter | MetricKind::Gauge => {
        let series = match ins.observed.get(&(name.clone(), label)) {
          Some(series) => Arc::clone(series),
          None => {
            let series: Series = Arc::default();
            self.observe(&name, key.kind(), &series);
            ins.observed.insert((name, label), Arc::clone(&series));
            series
          }
        };
        let mut s = series.write().unwrap_or_else(PoisonError::into_inner);
        if s.iter().all(|(k, _)| k != key) {
          s.push((key.clone(), attrs));
        }
      }
      MetricKind::Histogram(_) | MetricKind::Summary(_) => {
        let h = ins
          .histograms
          .entry((name.clone(), label))
          .or_insert_with(|| {
            let builder = self.meter.f64_histogram(name);
            match key.kind() {
              MetricKind::Histogram(b) => builder.with_boundaries(b.as_slice().to_vec()).build(),
              _ => builder.build(),
            }
          })
          .clone();
        self.push(key, Pushed::Histogram(h, attrs));
      }
      MetricKind::Frequency => {
        let c = ins
          .frequencies
          .entry(name.clone())
          .or_insert_with(|| self.meter.u64_counter(name).build())
          .clone();
        self.push(key, Pushed::Frequency(c, attrs));
      }
    }
  }

  fn push(&self, key: &MetricKey, pushed: Pushed) {
    self
      .pushed
      .write()
      .unwrap_or_else(PoisonError::into_inner)
      .entry(key.clone())
      .or_insert(pushed);
  }

  /// Builds the observable instrument for a pulled `(name, kind)`; its callback reads `series`.
  fn observe(self: &Arc<Self>, name: &str, kind: &MetricKind, series: &Series) {
    let weak = Arc::downgrade(self);
    let series = Arc::clone(series);
    match kind {
      MetricKind::Counter => {
        let _ = self
          .meter
          .u64_observable_counter(name.to_owned())
          .with_callback(move |obs| {
            let Some(shared) = weak.upgrade() else { return };
            for (key, attrs) in series.read().unwrap_or_else(PoisonError::into_inner).iter() {
              if let Some(MetricState::Counter { count }) = shared.registry.get(key) {
                obs.observe(count, attrs);
              }
            }
          })
          .build();
      }
      MetricKind::Gauge => {
        let _ = self
          .meter
          .i64_observable_gauge(name.to_owned())
          .with_callback(move |obs| {
            let Some(shared) = weak.upgrade() else { return };
            for (key, attrs) in series.read().unwrap_or_else(PoisonError::into_inner).iter() {
              if let Some(MetricState::Gauge { value }) = shared.registry.get(key) {
                obs.observe(value, attrs);
              }
            }
          })
          .build();
      }
      _ => {}
    }
  }

  fn record(self: &Arc<Self>, key: &MetricKey, update: MetricUpdate<'_>) {
    let (sample, value) = match update {
      MetricUpdate::Histogram(v) | MetricUpdate::Summary(v) => (Some(v), None),
      MetricUpdate::Frequency(s) => (None, Some(s)),
      // Counters and gauges are pulled.
      MetricUpdate::Counter(_) | MetricUpdate::Gauge(_) => return,
    };
    if !self.record_pushed(key, sample, value) {
      // A key created while the listener was being added: register it now.
      self.register(key);
      let _ = self.record_pushed(key, sample, value);
    }
  }

  fn record_pushed(&self, key: &MetricKey, sample: Option<f64>, value: Option<&str>) -> bool {
    let pushed = self.pushed.read().unwrap_or_else(PoisonError::into_inner);
    match (pushed.get(key), sample, value) {
      (Some(Pushed::Histogram(h, attrs)), Some(v), _) => h.record(v, attrs),
      (Some(Pushed::Frequency(c, attrs)), _, Some(v)) => {
        let mut attrs = attrs.clone();
        attrs.push(KeyValue::new(FREQUENCY_VALUE, v.to_owned()));
        c.add(1, &attrs);
      }
      (Some(_), _, _) => {}
      (None, _, _) => return false,
    }
    true
  }
}

/// The registry listener: a weak handle, so the registry never keeps the bridge alive.
struct Listener(Weak<Shared>);

impl MetricListener for Listener {
  fn on_register(&self, key: &MetricKey) {
    if let Some(shared) = self.0.upgrade() {
      shared.register(key);
    }
  }

  fn on_update(&self, key: &MetricKey, update: MetricUpdate<'_>) {
    if let Some(shared) = self.0.upgrade() {
      shared.record(key, update);
    }
  }
}

/// A [`MetricRegistry`] attached to an OpenTelemetry [`Meter`] by [`bridge_metrics`]: counters
/// and gauges are observable instruments read from the registry at collection; histograms,
/// summaries and frequencies are pushed by a registry listener. Cloning shares the bridge;
/// dropping the last clone detaches it.
#[derive(Clone)]
pub struct MetricsBridge {
  shared: Arc<Shared>,
}

impl std::fmt::Debug for MetricsBridge {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MetricsBridge")
      .field("registry", &self.shared.registry)
      .finish_non_exhaustive()
  }
}

impl MetricsBridge {
  /// The bridged registry.
  pub fn registry(&self) -> &MetricRegistry {
    &self.shared.registry
  }
}

/// Exports every series of `registry` through `meter` (pull for counters and gauges, push for
/// histograms, summaries and frequencies). Keep the returned bridge alive while exporting.
pub fn bridge_metrics(registry: &MetricRegistry, meter: Meter) -> MetricsBridge {
  let shared = Arc::new(Shared {
    registry: registry.clone(),
    meter,
    instruments: Mutex::default(),
    pushed: RwLock::default(),
  });
  registry.add_listener(Arc::new(Listener(Arc::downgrade(&shared))));
  MetricsBridge { shared }
}

/// [`bridge_metrics`] on the process registry ([`MetricRegistry::global`]), which
/// `Counter::new`, `Gauge::new`, ... record into.
pub fn bridge_global_metrics(meter: Meter) -> MetricsBridge {
  bridge_metrics(MetricRegistry::global(), meter)
}

/// Builds an [`SdkMeterProvider`] that exports metrics to an in-memory buffer (tests and spikes).
pub fn sdk_meter_provider_with_in_memory_exporter(
  exporter: &InMemoryMetricExporter,
) -> SdkMeterProvider {
  let reader = PeriodicReader::builder(exporter.clone()).build();
  SdkMeterProvider::builder().with_reader(reader).build()
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;
  use id_effect::next::{Boundaries, Effect, Metric, SummaryConfig, succeed};
  use opentelemetry::metrics::MeterProvider as _;
  use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};

  fn setup() -> (MetricRegistry, InMemoryMetricExporter, SdkMeterProvider) {
    let exporter = InMemoryMetricExporter::default();
    let provider = sdk_meter_provider_with_in_memory_exporter(&exporter);
    (MetricRegistry::new(), exporter, provider)
  }

  /// The metric named `name` in the latest export.
  fn exported<T>(
    exporter: &InMemoryMetricExporter,
    name: &str,
    f: impl FnOnce(&AggregatedMetrics) -> T,
  ) -> Option<T> {
    let finished = exporter.get_finished_metrics().expect("metrics");
    let last = finished.last()?;
    let metric = last
      .scope_metrics()
      .flat_map(|s| s.metrics())
      .find(|m| m.name() == name)?;
    Some(f(metric.data()))
  }

  fn has_attr<'a>(mut attrs: impl Iterator<Item = &'a KeyValue>, k: &str, v: &str) -> bool {
    attrs.any(|kv| kv.key.as_str() == k && kv.value.as_str() == v)
  }

  #[test]
  fn counter_is_pulled_from_the_registry_with_tags_as_attributes() {
    let (registry, exporter, provider) = setup();
    let requests = registry.counter("requests").tagged("route", "/health");
    // Recorded before the bridge exists: replayed on attach.
    requests.update_now(2);
    let _bridge = bridge_metrics(&registry, provider.meter("test"));
    let eff: Effect<(), ()> = requests.increment();
    assert!(matches!(eff.block_on(), Exit::Success(())));
    provider.force_flush().expect("flush");
    let points = exported(&exporter, "requests", |data| match data {
      AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
        .data_points()
        .map(|p| (p.value(), has_attr(p.attributes(), "route", "/health")))
        .collect::<Vec<_>>(),
      other => panic!("expected a u64 sum, got {other:?}"),
    })
    .expect("requests exported");
    assert_eq!(points, vec![(3, true)]);
    let _ = provider.shutdown();
  }

  #[test]
  fn fiber_metric_tags_become_a_distinct_series() {
    let (registry, exporter, provider) = setup();
    let _bridge = bridge_metrics(&registry, provider.meter("test"));
    let hits = registry.counter("hits");
    let eff: Effect<(), ()> = hits.increment().tag_metrics("env", "test");
    assert!(matches!(eff.block_on(), Exit::Success(())));
    provider.force_flush().expect("flush");
    let tagged = exported(&exporter, "hits", |data| match data {
      AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
        .data_points()
        .any(|p| p.value() == 1 && has_attr(p.attributes(), "env", "test")),
      _ => false,
    });
    assert_eq!(tagged, Some(true));
    let _ = provider.shutdown();
  }

  #[test]
  fn gauge_is_pulled_as_an_observable_gauge() {
    let (registry, exporter, provider) = setup();
    let _bridge = bridge_metrics(&registry, provider.meter("test"));
    let depth = registry.gauge("queue_depth");
    depth.update_now(7);
    depth.update_now(-3);
    provider.force_flush().expect("flush");
    let value = exported(&exporter, "queue_depth", |data| match data {
      AggregatedMetrics::I64(MetricData::Gauge(g)) => g.data_points().map(|p| p.value()).sum(),
      other => panic!("expected an i64 gauge, got {other:?}"),
    });
    assert_eq!(value, Some(-3));
    let _ = provider.shutdown();
  }

  #[test]
  fn histogram_samples_are_pushed_with_the_registry_boundaries() {
    let (registry, exporter, provider) = setup();
    let _bridge = bridge_metrics(&registry, provider.meter("test"));
    let latency = registry.histogram("latency_ms", Boundaries::new([10.0, 100.0]));
    let eff: Effect<u32, ()> = latency
      .observe(5.0)
      .then(latency.observe(50.0))
      .then(latency.observe(500.0))
      .then(succeed(1));
    assert!(matches!(eff.block_on(), Exit::Success(1)));
    provider.force_flush().expect("flush");
    let (bounds, counts, count, sum) = exported(&exporter, "latency_ms", |data| match data {
      AggregatedMetrics::F64(MetricData::Histogram(h)) => {
        let p = h.data_points().next().expect("one point");
        (
          p.bounds().collect::<Vec<_>>(),
          p.bucket_counts().collect::<Vec<_>>(),
          p.count(),
          p.sum(),
        )
      }
      other => panic!("expected an f64 histogram, got {other:?}"),
    })
    .expect("latency exported");
    assert_eq!(bounds, vec![10.0, 100.0]);
    assert_eq!(counts, vec![1, 1, 1]);
    assert_eq!(count, 3);
    assert!((sum - 555.0).abs() < 1e-9);
    let _ = provider.shutdown();
  }

  #[test]
  fn summary_and_frequency_are_pushed() {
    let (registry, exporter, provider) = setup();
    let _bridge = bridge_metrics(&registry, provider.meter("test"));
    let size = registry.summary(
      "payload_bytes",
      SummaryConfig::new(std::time::Duration::from_secs(60), 16, [0.5]),
    );
    size.update_now(42.0);
    let codes = registry.frequency("status");
    codes.update_now("200".to_owned());
    codes.update_now("200".to_owned());
    codes.update_now("500".to_owned());
    provider.force_flush().expect("flush");
    let summary_count = exported(&exporter, "payload_bytes", |data| match data {
      AggregatedMetrics::F64(MetricData::Histogram(h)) => h.data_points().map(|p| p.count()).sum(),
      _ => 0u64,
    });
    assert_eq!(summary_count, Some(1));
    let mut by_value = exported(&exporter, "status", |data| match data {
      AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
        .data_points()
        .map(|p| {
          let v = p
            .attributes()
            .find(|kv| kv.key.as_str() == FREQUENCY_VALUE)
            .map(|kv| kv.value.as_str().into_owned())
            .unwrap_or_default();
          (v, p.value())
        })
        .collect::<Vec<_>>(),
      other => panic!("expected a u64 sum, got {other:?}"),
    })
    .expect("status exported");
    by_value.sort();
    assert_eq!(by_value, vec![("200".into(), 2), ("500".into(), 1)]);
    let _ = provider.shutdown();
  }

  #[test]
  fn dropping_the_bridge_detaches_it() {
    let (registry, exporter, provider) = setup();
    let bridge = bridge_metrics(&registry, provider.meter("test"));
    let h = registry.histogram("dropped", Boundaries::new([1.0]));
    h.update_now(0.5);
    drop(bridge);
    h.update_now(0.5);
    registry.counter("after_drop").update_now(1);
    provider.force_flush().expect("flush");
    let count = exported(&exporter, "dropped", |data| match data {
      AggregatedMetrics::F64(MetricData::Histogram(h)) => h.data_points().map(|p| p.count()).sum(),
      _ => 0u64,
    });
    assert_eq!(count, Some(1));
    assert!(exported(&exporter, "after_drop", |_| ()).is_none());
    let _ = provider.shutdown();
  }
}
