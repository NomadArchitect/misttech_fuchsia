// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Sampling rate and aggregation intervals.

use num::Integer;
use std::fmt::{self, Debug, Display, Formatter};
use std::marker::PhantomData;
use std::{cmp, iter};

use crate::experimental::clock::{Duration, DurationExt as _, Quanta, QuantaExt as _, Tick};
use crate::experimental::series::interpolation::InterpolationState;
use crate::experimental::series::statistic::Statistic;
use crate::experimental::Vec1;

/// An interval that has elapsed during a [`Tick`].
///
/// [`Tick`]: crate::experimental::clock::Tick;
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ElapsedInterval {
    /// The number of fill (interpolated) samples required prior to computing the aggregation of
    /// the interval.
    fill_sample_count: usize,
}

impl ElapsedInterval {
    /// Fills the given [`Statistic`] with interpolated samples using the given
    /// [interpolation][`Interpolation`] and then computes the aggregation for the interval.
    ///
    /// [`Interpolation`]: crate::experimental::series::interpolation::Interpolation
    /// [`Statistic`]: crate::experimental::series::statistic::Statistic
    fn interpolate_and_get_aggregation<F, P>(
        self,
        statistic: &mut F,
        interpolation: &mut P,
    ) -> Result<Option<F::Aggregation>, F::Error>
    where
        F: Statistic,
        P: InterpolationState<F::Aggregation, FillSample = F::Sample>,
    {
        let ElapsedInterval { fill_sample_count } = self;
        let fill = interpolation.sample();

        statistic.fill(fill, fill_sample_count)?;
        Ok(statistic.aggregation().inspect(|aggregation| {
            interpolation.fold_aggregation(aggregation.clone());
        }))
    }
}

/// An interval that has been reached but **not** elapsed by a [`Tick`].
///
/// [`Tick`]: crate::experimental::clock::Tick;
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PendingInterval<T> {
    /// The number of fill (interpolated) samples required prior to folding the observed sample.
    fill_sample_count: usize,
    /// The sample observed during the tick.
    sample: T,
}

impl<T> PendingInterval<T> {
    /// Folds the observed sample into the given [`Statistic`] using the given
    /// [interpolation][`Interpolation`].
    ///
    /// [`Interpolation`]: crate::experimental::series::interpolation::Interpolation
    /// [`Statistic`]: crate::experimental::series::statistic::Statistic
    fn fold<F, P>(self, statistic: &mut F, interpolation: &mut P) -> Result<(), F::Error>
    where
        T: Clone,
        F: Statistic<Sample = T>,
        P: InterpolationState<F::Aggregation, FillSample = T>,
    {
        let PendingInterval { fill_sample_count, sample } = self;
        let fill = interpolation.sample();

        statistic.fill(fill, fill_sample_count)?;
        statistic.fold(sample.clone())?;
        interpolation.fold_sample(sample);
        Ok(())
    }
}

// A pending interval has no observed sample (`PhantomData<T>` instead of `T`) when an
// interpolation (rather than a fold) is requested.
impl<T> PendingInterval<PhantomData<T>> {
    /// Fills the given [`Statistic`] with interpolated samples using the given
    /// [interpolation][`Interpolation`].
    ///
    /// [`Interpolation`]: crate::experimental::series::interpolation::Interpolation
    /// [`Statistic`]: crate::experimental::series::statistic::Statistic
    fn interpolate<F, P>(self, statistic: &mut F, interpolation: &mut P) -> Result<(), F::Error>
    where
        T: Clone,
        F: Statistic<Sample = T>,
        P: InterpolationState<F::Aggregation, FillSample = T>,
    {
        let PendingInterval { fill_sample_count, .. } = self;
        let fill = interpolation.sample();

        Ok(statistic.fill(fill, fill_sample_count)?)
    }
}

/// The expiration of [`SamplingInterval`]s intersected by a [`Tick`].
///
/// [`SamplingInterval`]: crate::experimental::series::SamplingInterval
/// [`Tick`]: crate::experimental::clock::Tick;
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IntervalExpiration<T> {
    Elapsed(ElapsedInterval),
    Pending(PendingInterval<T>),
}

impl<T> IntervalExpiration<T> {
    pub(crate) fn fold_and_get_aggregation<F, P>(
        self,
        statistic: &mut F,
        interpolation: &mut P,
    ) -> Result<Option<F::Aggregation>, F::Error>
    where
        T: Clone,
        F: Statistic<Sample = T>,
        P: InterpolationState<F::Aggregation, FillSample = T>,
    {
        match self {
            IntervalExpiration::Elapsed(elapsed) => {
                elapsed.interpolate_and_get_aggregation(statistic, interpolation)
            }
            IntervalExpiration::Pending(pending) => {
                pending.fold(statistic, interpolation).map(|_| None)
            }
        }
    }
}

impl<T> IntervalExpiration<PhantomData<T>> {
    pub(crate) fn interpolate_and_get_aggregation<F, P>(
        self,
        statistic: &mut F,
        interpolation: &mut P,
    ) -> Result<Option<F::Aggregation>, F::Error>
    where
        T: Clone,
        F: Statistic<Sample = T>,
        P: InterpolationState<F::Aggregation, FillSample = T>,
    {
        match self {
            IntervalExpiration::Elapsed(elapsed) => {
                elapsed.interpolate_and_get_aggregation(statistic, interpolation)
            }
            IntervalExpiration::Pending(pending) => {
                pending.interpolate(statistic, interpolation).map(|_| None)
            }
        }
    }
}

/// A time interval in which samples are folded into an aggregation.
///
/// Sampling intervals determine the timing of aggregations and interpolation in time series and
/// are defined by the following quantities:
///
///   1. **Maximum sampling period.** This is the basic unit of time that defines the sampling
///      interval and represents the maximum duration in which a sample must be observed. For any
///      such duration in which no sample is observed, an interpolated sample is used instead. This
///      can also be thought of as its inverse: the minimum sampling frequency.
///   2. **Sampling period count.** This is the number of sampling periods that form the sampling
///      interval. This determines the minimum number of samples (interpolated or otherwise) folded
///      into the aggregation for the sampling interval.
///   3. **Capacity.** This affects the number of sampling intervals (and therefore aggregations)
///      that are stored to represent an aggregated series. The number of sampling intervals that
///      are stored are dependent on the buffer implementation and the compression ratio. This
///      quantity is somewhat extrinsic to the time interval itself.
///
/// These quantities are concatenated into a shorthand to describe sampling intervals, formatted
/// as `capacity x sampling_period_count x maximum_sampling_period`. For example, a 10x2x5s
/// sampling interval persists 10 intervals formed from two maximum sampling period of 5s. In such
/// an interval, there is at least one sample every 5s, an aggregation every 10s, and at most 10
/// aggregations that represent a 100s period.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SamplingInterval {
    capacity: u32,
    sampling_period_count: u32,
    max_sampling_period: Quanta,
}

impl SamplingInterval {
    fn new(
        capacity: u32,
        sampling_period_count: u32,
        max_sampling_period: impl Into<Duration>,
    ) -> Self {
        SamplingInterval {
            capacity: cmp::max(1, capacity),
            sampling_period_count: cmp::max(1, sampling_period_count),
            max_sampling_period: cmp::max(1, max_sampling_period.into().into_quanta().abs()),
        }
    }

    /// Gets the duration of the interval (also known as the aggregation period).
    pub fn duration(&self) -> Duration {
        self.max_sampling_period() * self.sampling_period_count
    }

    /// Gets the maximum sampling period of the interval.
    pub fn max_sampling_period(&self) -> Duration {
        Duration::from_quanta(self.max_sampling_period)
    }

    /// Gets the [expirations][`IntervalExpiration`] of intervals intersected by the given
    /// [`Tick`].
    ///
    /// The given sample is always folded into exactly one pending interval that terminates the
    /// sequence.
    ///
    /// [`IntervalExpiration`]: crate::experimental::series::interval::IntervalExpiration
    /// [`Tick`]: crate::experimental::clock::Tick;
    pub(crate) fn fold_and_get_expirations<T>(
        &self,
        tick: Tick,
        sample: T,
    ) -> impl Clone + Iterator<Item = IntervalExpiration<T>>
    where
        T: Clone,
    {
        let interval = self.max_sampling_period * Quanta::from(self.sampling_period_count);
        let (start, end) = tick.quantize();
        let start_has_sample = tick.start_has_sample(self.max_sampling_period);

        // The intervals intersected by this `Tick` are constructed from three groups:
        //
        //   - Zero or one _resumed_ intervals. Such an interval was previously the _pending_
        //     interval (see below), but has been elapsed by this `Tick`.
        //   - Zero or more _skipped_ intervals. Such intervals were never previously intersected,
        //     but are elapsed by this `Tick`.
        //   - Exactly one _pending_ interval. This is the interval intersected by the end
        //     timestamp of this `Tick`. There is always such an interval and this interval may
        //     have been pending previously.
        //
        // Note that all quantities here are technically periods or durations: start and end
        // timestamps are durations from zero, for example. Divisions yield unitless quantities
        // (counts).
        //
        // About the below calculation: Buckets are always aligned, so we can simply divide
        // timestamps by an `interval` or `max_sampling_period` to find out whether they fall into
        // the same interval or max_sampling_period.
        //
        // For example, if the interval is 60s, timestamps at 1s and 30s marks would both into
        // the [0, 60) interval. OTOH, timestamp at 61s mark would fall into the [60, 120) interval.
        // We see that `1 / 60 == 30 / 60` (integer division), whereas `1 / 60 != 61 / 60`.
        let resumed_interval_has_elapsed = (end / interval) > (start / interval);
        let num_skipped_intervals = (end / interval) - Integer::div_ceil(&start, &interval);

        let pending_interval_fill_sample_count = if resumed_interval_has_elapsed {
            // If the pending interval is a new one, simply calculate how many sampling periods
            // it covers.
            (end % interval) / self.max_sampling_period
        } else {
            // If the pending interval is an existing one, check how many sampling periods exist
            // between start and end.
            let num_elapsed_sampling_periods =
                (end / self.max_sampling_period) - (start / self.max_sampling_period);
            // Adjust the result based on whether the sampling period for the start timestamp
            // already had a sample.
            // This calculation can yield negative number if `start` and `end` are in the same
            // sampling period, but we'll change it to 0 when cast to usize later on.
            num_elapsed_sampling_periods - if start_has_sample { 1 } else { 0 }
        };
        itertools::chain!(
            resumed_interval_has_elapsed.then(|| {
                // Calculate how many sampling periods the remaining duration of the resumed
                // interval covers. Adjust the result based on whether the sampling period for
                // the start timestamp already had a sample.
                let resumed_interval_remaining = interval - (start % interval);
                let resumed_interval_fill_sample_count =
                    Integer::div_ceil(&resumed_interval_remaining, &self.max_sampling_period)
                        - if start_has_sample { 1 } else { 0 };
                IntervalExpiration::Elapsed(ElapsedInterval {
                    fill_sample_count: usize::try_from(resumed_interval_fill_sample_count)
                        .unwrap_or(0),
                })
            }),
            iter::repeat(IntervalExpiration::Elapsed(ElapsedInterval {
                fill_sample_count: usize::try_from(self.sampling_period_count)
                    .unwrap_or(usize::MAX),
            }))
            .take(usize::try_from(num_skipped_intervals).unwrap_or(0)),
            // The pending interval falls on the end timestamp and so includes the associated
            // sample.
            Some(IntervalExpiration::Pending(PendingInterval {
                fill_sample_count: usize::try_from(pending_interval_fill_sample_count).unwrap_or(0),
                sample,
            })),
        )
    }
}

impl Display for SamplingInterval {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}x{}x{}",
            self.capacity,
            self.sampling_period_count,
            self.max_sampling_period.into_nearest_unit_display(),
        )
    }
}

/// One or more cooperative [`SamplingInterval`]s.
#[derive(Clone, Debug)]
pub struct SamplingProfile(Vec1<SamplingInterval>);

impl SamplingProfile {
    fn from_sampling_intervals<I>(intervals: I) -> Self
    where
        Vec1<SamplingInterval>: From<I>,
    {
        SamplingProfile(intervals.into())
    }

    /// Constructs a granular sampling profile with high fidelity but low durability.
    ///
    /// The minimum granularity is 1s and the maximum durability is 20m.
    pub fn granular() -> Self {
        SamplingProfile::from_sampling_intervals([
            // 120x1x10s
            SamplingInterval::new(120, 1, Duration::from_seconds(10)),
            // 120x1x1s
            SamplingInterval::new(120, 1, Duration::from_seconds(1)),
        ])
    }

    /// Constructs a sampling profile with fidelity and durability that is applicable to most
    /// metrics.
    ///
    /// The minimum granularity is 10s and the maximum durability is 5d.
    pub fn balanced() -> Self {
        SamplingProfile::from_sampling_intervals([
            // 120x1x1h
            SamplingInterval::new(120, 1, Duration::from_hours(1)),
            // 120x1x10m
            SamplingInterval::new(120, 1, Duration::from_minutes(10)),
            // 120x1x1m
            SamplingInterval::new(120, 1, Duration::from_minutes(1)),
            // 120x1x10s
            SamplingInterval::new(120, 1, Duration::from_seconds(10)),
        ])
    }

    /// Constructs a durable sampling profile with high historical longevity but low fidelity.
    ///
    /// The minimum granularity is 1h and the maximum durability is 60d.
    pub fn durable() -> Self {
        SamplingProfile::from_sampling_intervals([
            // 120x1x12h
            SamplingInterval::new(120, 1, Duration::from_hours(12)),
            // 120x1x1h
            SamplingInterval::new(120, 1, Duration::from_hours(1)),
        ])
    }

    /// Gets the minimum granularity of the profile.
    pub fn granularity(&self) -> Duration {
        self.0.iter().map(SamplingInterval::max_sampling_period).min().unwrap()
    }

    pub(crate) fn into_sampling_intervals(self) -> Vec1<SamplingInterval> {
        self.0
    }
}

impl Default for SamplingProfile {
    fn default() -> Self {
        SamplingProfile::balanced()
    }
}

impl From<SamplingInterval> for SamplingProfile {
    fn from(interval: SamplingInterval) -> Self {
        SamplingProfile(Vec1::from_item(interval))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experimental::clock::{ObservationTime, Timestamp};
    use crate::experimental::series::{Counter, Fill, Sampler};

    const SAMPLE: u64 = 1337u64;

    #[test]
    fn sampling_interval_fold_and_get_expirations() {
        let sampling_interval = SamplingInterval::new(120, 6, Duration::from_seconds(10));
        let mut last = ObservationTime {
            last_update_timestamp: Timestamp::from_nanos(71_000_000_000),
            last_sample_timestamp: Timestamp::from_nanos(-1),
        };

        // Tick in the same sampling period that did not have a sample.
        // (last sample at -1 nano)
        let tick = last.tick(Timestamp::from_nanos(75_000_000_000), true).unwrap();
        let expirations: Vec<_> =
            sampling_interval.fold_and_get_expirations(tick, SAMPLE).collect();
        assert_eq!(
            expirations,
            vec![IntervalExpiration::Pending(PendingInterval {
                fill_sample_count: 0,
                sample: SAMPLE,
            })]
        );

        // Tick in the same sampling period that already had a sample.
        // (last sample at 75s)
        let tick = last.tick(Timestamp::from_nanos(79_000_000_000), false).unwrap();
        let expirations: Vec<_> =
            sampling_interval.fold_and_get_expirations(tick, SAMPLE).collect();
        assert_eq!(
            expirations,
            vec![IntervalExpiration::Pending(PendingInterval {
                fill_sample_count: 0,
                sample: SAMPLE,
            })]
        );

        // Tick to a new sampling period, but in the same interval. The sampling period
        // at the start of the tick already had a sample.
        let tick = last.tick(Timestamp::from_nanos(83_000_000_000), false).unwrap();
        let expirations: Vec<_> =
            sampling_interval.fold_and_get_expirations(tick, SAMPLE).collect();
        assert_eq!(
            expirations,
            vec![IntervalExpiration::Pending(PendingInterval {
                fill_sample_count: 0,
                sample: SAMPLE,
            })]
        );

        // Tick to a new sampling period, but in the same interval. The sampling period
        // at the start of the tick did not have a sample.
        let tick = last.tick(Timestamp::from_nanos(91_000_000_000), true).unwrap();
        let expirations: Vec<_> =
            sampling_interval.fold_and_get_expirations(tick, SAMPLE).collect();
        assert_eq!(
            expirations,
            vec![IntervalExpiration::Pending(PendingInterval {
                fill_sample_count: 1,
                sample: SAMPLE,
            })]
        );

        // Tick to a new interval. The sampling period at the start of the tick already
        // had a sample.
        let tick = last.tick(Timestamp::from_nanos(133_000_000_000), false).unwrap();
        let expirations: Vec<_> =
            sampling_interval.fold_and_get_expirations(tick, SAMPLE).collect();
        let expected = vec![
            IntervalExpiration::Elapsed(ElapsedInterval { fill_sample_count: 2 }),
            IntervalExpiration::Pending(PendingInterval { fill_sample_count: 1, sample: SAMPLE }),
        ];
        assert_eq!(expirations, expected);

        // Tick to a new interval. The sampling period at the start of the tick did not
        // have a sample. Additionally, there are some skipped intervals in-between
        let tick = last.tick(Timestamp::from_nanos(240_000_000_000), false).unwrap();
        let expirations: Vec<_> =
            sampling_interval.fold_and_get_expirations(tick, SAMPLE).collect();
        let expected = vec![
            IntervalExpiration::Elapsed(ElapsedInterval { fill_sample_count: 5 }),
            IntervalExpiration::Elapsed(ElapsedInterval { fill_sample_count: 6 }),
            IntervalExpiration::Pending(PendingInterval { fill_sample_count: 0, sample: SAMPLE }),
        ];
        assert_eq!(expirations, expected);
    }

    #[derive(Clone, Debug, PartialEq)]
    enum MockStatisticCall {
        Fill { sample: u64, n: usize },
        Fold { sample: u64 },
        Aggregation,
    }

    #[derive(Clone, Debug)]
    struct MockStatistic(Vec<MockStatisticCall>);
    impl MockStatistic {
        fn new() -> Self {
            Self(vec![])
        }
    }

    impl Statistic for MockStatistic {
        type Semantic = Counter<u64>;
        type Sample = u64;
        type Aggregation = u64;

        fn aggregation(&mut self) -> Option<Self::Aggregation> {
            self.0.push(MockStatisticCall::Aggregation);
            Some(100)
        }
    }

    impl Fill<u64> for MockStatistic {
        fn fill(&mut self, sample: u64, n: usize) -> Result<(), Self::Error> {
            self.0.push(MockStatisticCall::Fill { sample, n });
            Ok(())
        }
    }

    impl Sampler<u64> for MockStatistic {
        type Error = ();
        fn fold(&mut self, sample: u64) -> Result<(), Self::Error> {
            self.0.push(MockStatisticCall::Fold { sample });
            Ok(())
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    enum MockInterpolationStateCall {
        FoldSample { sample: u64 },
        FoldAggregation { aggregation: u64 },
    }

    #[derive(Clone, Debug)]
    struct MockInterpolationState(Vec<MockInterpolationStateCall>);
    impl MockInterpolationState {
        fn new() -> Self {
            Self(vec![])
        }
    }

    impl InterpolationState<u64> for MockInterpolationState {
        type FillSample = u64;
        fn sample(&self) -> Self::FillSample {
            42u64
        }
        fn fold_sample(&mut self, sample: Self::FillSample) {
            self.0.push(MockInterpolationStateCall::FoldSample { sample });
        }
        fn fold_aggregation(&mut self, sample: Self::FillSample) {
            self.0.push(MockInterpolationStateCall::FoldAggregation { aggregation: sample });
        }
    }

    #[test]
    fn elapsed_interval_interpolate_and_get_aggregation() {
        let interval = ElapsedInterval { fill_sample_count: 6 };
        let mut statistic = MockStatistic::new();
        let mut interpolation = MockInterpolationState::new();
        let result = interval.interpolate_and_get_aggregation(&mut statistic, &mut interpolation);
        assert!(result.is_ok());
        assert_eq!(
            statistic.0,
            vec![MockStatisticCall::Fill { sample: 42, n: 6 }, MockStatisticCall::Aggregation,]
        );
        assert_eq!(
            interpolation.0,
            vec![MockInterpolationStateCall::FoldAggregation { aggregation: 100 }]
        );
    }

    #[test]
    fn pending_interval_fold() {
        let interval = PendingInterval { fill_sample_count: 6, sample: 50u64 };
        let mut statistic = MockStatistic::new();
        let mut interpolation = MockInterpolationState::new();
        let result = interval.fold(&mut statistic, &mut interpolation);
        assert!(result.is_ok());
        assert_eq!(
            statistic.0,
            vec![
                MockStatisticCall::Fill { sample: 42, n: 6 },
                MockStatisticCall::Fold { sample: 50 },
            ]
        );
        assert_eq!(interpolation.0, vec![MockInterpolationStateCall::FoldSample { sample: 50 }]);
    }
}