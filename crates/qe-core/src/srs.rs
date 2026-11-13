//! 记忆曲线（间隔重复）算法。
//!
//! 采用 Anki 风格的“学习步骤 + SM-2 难度因子”混合模型：
//!
//! * 新卡先进入 learning 阶段，三个步长：10 分钟、1 天、3 天；
//! * 毕业（graduation）后进入 review 阶段，间隔按难度因子 `ease` 指数增长；
//! * 忘记（again）会扣 `ease` 并进入 relearning，重新巩固后回到 review；
//! * 所有间隔上限 365 天，`ease` 限制在 1.3 ~ 3.0。
//!
//! 这套规则与扇贝/Anki 的“记忆曲线”一致：短间隔重复 → 逐步拉长 → 遗忘后回调。

use chrono::{DateTime, Duration, Utc};

use crate::model::{Phase, Rating, SrsCard};

/// 新卡的学习步骤（分钟）。
pub const LEARNING_STEPS_MINUTES: [i64; 3] = [10, 24 * 60, 3 * 24 * 60];
/// 遗忘后的重学步骤（分钟）。
pub const RELEARNING_STEPS_MINUTES: [i64; 2] = [10, 24 * 60];
/// 毕业后第一次复习的间隔（天）。
pub const GRADUATING_INTERVAL: u32 = 3;
/// 直接按“简单”毕业的间隔（天）。
pub const EASY_INTERVAL: u32 = 5;
/// 默认目标回忆率，先记录在卡片上，供后续 FSRS 调度使用。
pub const DESIRED_RETENTION: f64 = 0.9;

const MIN_EASE: f64 = 1.3;
const MAX_EASE: f64 = 3.0;
const MAX_INTERVAL: u32 = 365;

fn clamp_ease(ease: f64) -> f64 {
    ease.clamp(MIN_EASE, MAX_EASE)
}

fn cap_interval(days: u32) -> u32 {
    days.clamp(1, MAX_INTERVAL)
}

/// 根据上一次状态与本次评价，算出下一次复习时间。
///
/// 对从未学过的新词传入 `None`。
pub fn review(prev: Option<&SrsCard>, rating: Rating, now: DateTime<Utc>) -> SrsCard {
    let mut card = prev
        .cloned()
        .unwrap_or_else(|| SrsCard::new(now));

    let elapsed_days = card
        .last_review_at
        .and_then(|last| (now - last).num_seconds().checked_add(86_399))
        .map(|seconds| (seconds / 86_400).max(0) as u32)
        .unwrap_or(0);
    card.reps = card.reps.saturating_add(1);
    card.last_review_at = Some(now);

    match card.phase {
        Phase::New => apply_new(&mut card, rating, now),
        Phase::Learning => apply_learning(&mut card, rating, now),
        Phase::Review => apply_review(&mut card, rating, now, elapsed_days),
        Phase::Relearning => apply_relearning(&mut card, rating, now),
    }

    card.ease = clamp_ease(card.ease);
    update_memory_state(&mut card, rating, elapsed_days);
    card
}

fn apply_new(card: &mut SrsCard, rating: Rating, now: DateTime<Utc>) {
    match rating {
        Rating::Again => {
            card.phase = Phase::Learning;
            card.step = 0;
            card.due_at = now + minutes(LEARNING_STEPS_MINUTES[0]);
        }
        Rating::Hard => {
            card.phase = Phase::Learning;
            card.step = 0;
            card.ease -= 0.15;
            card.due_at = now + minutes(LEARNING_STEPS_MINUTES[0]);
        }
        Rating::Good => {
            card.phase = Phase::Learning;
            card.step = 1;
            card.due_at = now + minutes(LEARNING_STEPS_MINUTES[1]);
        }
        Rating::Easy => {
            card.phase = Phase::Review;
            card.step = 0;
            card.interval_days = EASY_INTERVAL;
            card.due_at = now + days(EASY_INTERVAL);
        }
    }
}

fn apply_learning(card: &mut SrsCard, rating: Rating, now: DateTime<Utc>) {
    let current_step = (card.step as usize).min(LEARNING_STEPS_MINUTES.len() - 1);
    match rating {
        Rating::Again => {
            card.step = 0;
            card.due_at = now + minutes(LEARNING_STEPS_MINUTES[0]);
        }
        Rating::Hard => {
            card.due_at = now + minutes(LEARNING_STEPS_MINUTES[current_step]);
        }
        Rating::Good => {
            let next = current_step + 1;
            if next >= LEARNING_STEPS_MINUTES.len() {
                graduate(card, now, 0.0, 0);
            } else {
                card.step = next as u8;
                card.due_at = now + minutes(LEARNING_STEPS_MINUTES[next]);
            }
        }
        Rating::Easy => {
            card.ease += 0.15;
            graduate(card, now, 0.0, 0);
        }
    }
}

fn apply_review(card: &mut SrsCard, rating: Rating, now: DateTime<Utc>, elapsed_days: u32) {
    let interval = card.interval_days.max(1);
    // A late review is evidence that the card remained available longer than
    // planned. Never shorten an interval just because the user reviewed late.
    let effective_interval = interval.max(elapsed_days);
    match rating {
        Rating::Again => {
            card.phase = Phase::Relearning;
            card.step = 0;
            card.lapses = card.lapses.saturating_add(1);
            card.ease -= 0.2;
            // A lapse should hurt, but a single forgotten card should not be
            // sent all the way back to the beginning of its history.
            card.interval_days = cap_interval(((interval as f64) * 0.7).round() as u32);
            card.due_at = now + minutes(RELEARNING_STEPS_MINUTES[0]);
        }
        Rating::Hard => {
            card.ease -= 0.15;
            card.interval_days = cap_interval(((effective_interval as f64) * 1.2).round() as u32);
            card.due_at = now + days(card.interval_days);
        }
        Rating::Good => {
            let next = ((effective_interval as f64) * card.ease).round() as u32;
            card.interval_days = cap_interval(next.max(effective_interval + 1));
            card.due_at = now + days(card.interval_days);
        }
        Rating::Easy => {
            card.ease += 0.15;
            let next = ((effective_interval as f64) * card.ease * 1.3).round() as u32;
            card.interval_days = cap_interval(next.max(effective_interval + 2));
            card.due_at = now + days(card.interval_days);
        }
    }
}

fn apply_relearning(card: &mut SrsCard, rating: Rating, now: DateTime<Utc>) {
    let current_step = (card.step as usize).min(RELEARNING_STEPS_MINUTES.len() - 1);
    match rating {
        Rating::Again => {
            card.step = 0;
            card.due_at = now + minutes(RELEARNING_STEPS_MINUTES[0]);
        }
        Rating::Hard => {
            card.step = current_step as u8;
            card.due_at = now + minutes(RELEARNING_STEPS_MINUTES[current_step]);
        }
        Rating::Good => {
            let next = current_step + 1;
            if next >= RELEARNING_STEPS_MINUTES.len() {
                graduate(card, now, 0.0, 0);
            } else {
                card.step = next as u8;
                card.due_at = now + minutes(RELEARNING_STEPS_MINUTES[next]);
            }
        }
        Rating::Easy => {
            card.ease += 0.1;
            graduate(card, now, 2.0, 2);
        }
    }
}

/// Keep the data needed by a future stability/difficulty scheduler up to date
/// without changing the current interval formula yet.
fn update_memory_state(card: &mut SrsCard, rating: Rating, elapsed_days: u32) {
    if card.stability <= 0.0 {
        card.stability = card.interval_days.max(1) as f64;
    }
    if elapsed_days > 0 {
        card.stability = card.stability.max(elapsed_days as f64);
    }
    card.difficulty = (card.difficulty
        + match rating {
            Rating::Again => 0.8,
            Rating::Hard => 0.3,
            Rating::Good => -0.1,
            Rating::Easy => -0.3,
        })
    .clamp(1.0, 10.0);
    card.desired_retention = card.desired_retention.clamp(0.7, 0.99);
}

/// 离开 learning/relearning，进入 review。
/// `bonus_ratio` 用于评价为“简单”时额外放大间隔。
fn graduate(card: &mut SrsCard, now: DateTime<Utc>, _bias: f64, extra_days: u32) {
    let base = card.interval_days.max(GRADUATING_INTERVAL);
    card.phase = Phase::Review;
    card.step = 0;
    card.interval_days = cap_interval(base.saturating_add(extra_days));
    card.due_at = now + days(card.interval_days);
}

fn minutes(n: i64) -> Duration {
    Duration::minutes(n)
}

fn days(n: u32) -> Duration {
    Duration::days(n as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time;

    fn at(hour: u32) -> DateTime<Utc> {
        time::utc(2026, 11, 10, hour)
    }

    #[test]
    fn new_card_good_moves_to_learning() {
        let card = review(None, Rating::Good, at(9));
        assert_eq!(card.phase, Phase::Learning);
        assert_eq!(card.step, 1);
        assert_eq!(card.reps, 1);
        // 第二步是 1 天
        assert!(card.due_at > at(9) + Duration::hours(23));
    }

    #[test]
    fn new_card_again_stays_today() {
        let card = review(None, Rating::Again, at(9));
        assert_eq!(card.phase, Phase::Learning);
        assert_eq!(card.step, 0);
        assert!(card.due_at <= at(10));
    }

    #[test]
    fn easy_graduates_immediately() {
        let card = review(None, Rating::Easy, at(9));
        assert_eq!(card.phase, Phase::Review);
        assert_eq!(card.interval_days, EASY_INTERVAL);
    }

    #[test]
    fn review_good_grows_interval() {
        let mut card = SrsCard::new(at(9));
        card.phase = Phase::Review;
        card.interval_days = 10;
        card.ease = 2.5;
        let next = review(Some(&card), Rating::Good, at(9));
        assert_eq!(next.phase, Phase::Review);
        assert_eq!(next.interval_days, 25);
    }

    #[test]
    fn review_again_enters_relearning() {
        let mut card = SrsCard::new(at(9));
        card.phase = Phase::Review;
        card.interval_days = 10;
        card.ease = 2.5;
        let next = review(Some(&card), Rating::Again, at(9));
        assert_eq!(next.phase, Phase::Relearning);
        assert_eq!(next.lapses, 1);
        assert_eq!(next.interval_days, 7);
        assert!(next.ease < card.ease);
    }

    #[test]
    fn late_review_uses_actual_elapsed_interval() {
        let mut card = SrsCard::new(at(9));
        card.phase = Phase::Review;
        card.interval_days = 10;
        card.ease = 2.5;
        card.last_review_at = Some(at(9));
        let next = review(Some(&card), Rating::Good, at(9) + Duration::days(11));
        assert_eq!(next.interval_days, 27);
        assert_eq!(next.stability, 11.0);
    }

    #[test]
    fn releasing_then_good_returns_to_review() {
        let mut card = SrsCard::new(at(9));
        card.phase = Phase::Relearning;
        card.interval_days = 5;
        card.ease = 2.2;
        let next = review(Some(&card), Rating::Good, at(10));
        assert_eq!(next.phase, Phase::Relearning);
        let graduated = review(Some(&next), Rating::Good, at(20));
        assert_eq!(graduated.phase, Phase::Review);
        assert!(graduated.interval_days >= 3);
    }

    #[test]
    fn ease_is_clamped() {
        let mut card = SrsCard::new(at(9));
        card.phase = Phase::Review;
        card.interval_days = 3;
        card.ease = MIN_EASE;
        let next = review(Some(&card), Rating::Again, at(9));
        assert!(next.ease >= MIN_EASE);
    }

    #[test]
    fn interval_is_capped() {
        let mut card = SrsCard::new(at(9));
        card.phase = Phase::Review;
        card.interval_days = 300;
        card.ease = 3.0;
        let next = review(Some(&card), Rating::Easy, at(9));
        assert!(next.interval_days <= MAX_INTERVAL);
    }

    #[test]
    fn learning_steps_progress() {
        // 新卡评价“认识” → 进入第二步（1 天）
        let first = review(None, Rating::Good, at(9));
        // 第二步再评价“认识” → 进入第三步（3 天）
        let second = review(Some(&first), Rating::Good, at(20));
        assert_eq!(second.phase, Phase::Learning);
        assert_eq!(second.step, 2);
        // 第三步再评价“认识” → 毕业
        let third = review(Some(&second), Rating::Good, at(20));
        assert_eq!(third.phase, Phase::Review);
        assert!(third.interval_days >= GRADUATING_INTERVAL);
    }
}
