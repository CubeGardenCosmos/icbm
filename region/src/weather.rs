//! 天气状态机（对照 `ServerWorld.tickWeather` 的简化模型）。
//!
//! - `clear_time`/`rain_time`/`thunder_time` 倒计时切换晴/雨/雷（受
//!   `doWeatherCycle` 控制）；
//! - `rain_gradient`/`thunder_gradient` 向目标（雨/雷→1，否则→0）每 tick
//!   增减 0.01（对照 vanilla `ServerWorld` 的梯度插值）；
//! - 梯度变化广播 `GAME_EVENT RAIN_GRADIENT_CHANGED(7)/THUNDER_GRADIENT_CHANGED(8)`；
//!   晴雨切换广播 `RAIN_STARTED(1)/RAIN_STOPPED(2)`。

/// 晴雨切换倒计时取值范围（tick，对照 `ServerWorld` 的
/// `CLEAR_WEATHER_DURATION_PROVIDER`/`RAIN_WEATHER_DURATION_PROVIDER`）。
const CLEAR_WEATHER_MIN: i64 = 12_000;
const CLEAR_WEATHER_MAX: i64 = 24_000;
/// 雷暴切换倒计时（`CLEAR_THUNDER_DURATION_PROVIDER`/`THUNDER_WEATHER_DURATION_PROVIDER`）。
const CLEAR_THUNDER_MIN: i64 = 12_000;
const CLEAR_THUNDER_MAX: i64 = 18_000;
const THUNDER_MIN: i64 = 3_600;
const THUNDER_MAX: i64 = 15_600;

/// 天气状态。
#[derive(Debug)]
pub struct WeatherState {
    /// 是否下雨。
    raining: bool,
    /// 是否雷暴。
    thundering: bool,
    /// 雨量梯度（0..1）。
    rain_gradient: f32,
    /// 雷量梯度（0..1）。
    thunder_gradient: f32,
    /// 晴天剩余（tick；>0 时强制晴）。
    clear_time: i64,
    /// 当前雨态剩余（tick）。
    rain_time: i64,
    /// 当前雷态剩余（tick）。
    thunder_time: i64,
}

impl WeatherState {
    /// 新建（晴天，雨/雷梯度 0）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            raining: false,
            thundering: false,
            rain_gradient: 0.0,
            thunder_gradient: 0.0,
            clear_time: 0,
            rain_time: Self::random_duration(CLEAR_WEATHER_MIN, CLEAR_WEATHER_MAX, 0x9E37_79B9),
            thunder_time: Self::random_duration(CLEAR_THUNDER_MIN, CLEAR_THUNDER_MAX, 0xBF58_476D),
        }
    }

    /// xorshift 简化随机时长。
    fn random_duration(min: i64, max: i64, mut seed: u64) -> i64 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        min + (seed % u64::try_from(max - min + 1).unwrap_or(1)) as i64
    }

    /// 当前是否下雨。
    #[must_use]
    pub const fn is_raining(&self) -> bool {
        self.raining
    }

    /// 直接设置晴雨/雷暴目标（core 全局同步用；`/weather` 命令）。
    pub const fn set(&mut self, raining: bool, thundering: bool) {
        self.raining = raining;
        self.thundering = thundering;
    }

    /// 应用 core 同步的完整天气状态（含梯度）；返回晴雨是否切换。
    pub const fn apply_from_core(&mut self, raining: bool, thundering: bool, rain: f32, thunder: f32) -> bool {
        let toggled = raining != self.raining;
        self.raining = raining;
        self.thundering = thundering;
        // clamp 手动实现（保持 const fn 兼容）
        self.rain_gradient = if rain < 0.0 { 0.0 } else if rain > 1.0 { 1.0 } else { rain };
        self.thunder_gradient = if thunder < 0.0 {
            0.0
        } else if thunder > 1.0 {
            1.0
        } else {
            thunder
        };
        toggled
    }

    /// 当前是否雷暴。
    #[must_use]
    pub const fn is_thundering(&self) -> bool {
        self.thundering
    }

    /// 雨量梯度（进服同步用）。
    #[must_use]
    pub const fn rain_gradient(&self) -> f32 {
        self.rain_gradient
    }

    /// 雷量梯度（进服同步用）。
    #[must_use]
    pub const fn thunder_gradient(&self) -> f32 {
        self.thunder_gradient
    }

    /// 推进一个 tick；返回需要广播的天气事件。
    ///
    /// 返回 `Vec<(u8, f32)>`：`GAME_EVENT` 的 `(event, value)` 序列（含
    /// 梯度变化与晴雨切换；雷暴切换仅体现在雷梯度变化上，对照 vanilla）。
    ///
    /// `do_weather_cycle=false` 时只做梯度插值（已在下雨则维持雨），不推进
    /// 状态切换（对照 vanilla `tickWeather` 的 gamerule 分支）。
    pub fn tick(&mut self, do_weather_cycle: bool) -> Vec<(u8, f32)> {
        let mut events: Vec<(u8, f32)> = Vec::new();
        if do_weather_cycle {
            let mut thundering = self.thundering;
            let mut raining = self.raining;
            if self.clear_time > 0 {
                self.clear_time -= 1;
                self.thunder_time = i64::from(!thundering);
                self.rain_time = i64::from(!raining);
                thundering = false;
                raining = false;
            } else {
                if self.thunder_time > 0 {
                    self.thunder_time -= 1;
                    if self.thunder_time == 0 {
                        thundering = !thundering;
                    }
                } else if thundering {
                    self.thunder_time = Self::random_duration(THUNDER_MIN, THUNDER_MAX, 0xA5A5_5A5A);
                } else {
                    self.thunder_time =
                        Self::random_duration(CLEAR_THUNDER_MIN, CLEAR_THUNDER_MAX, 0x5A5A_A5A5);
                }
                if self.rain_time > 0 {
                    self.rain_time -= 1;
                    if self.rain_time == 0 {
                        raining = !raining;
                    }
                } else if raining {
                    self.rain_time =
                        Self::random_duration(CLEAR_WEATHER_MIN, CLEAR_WEATHER_MAX, 0xDEAD_BEEF);
                } else {
                    self.rain_time =
                        Self::random_duration(CLEAR_WEATHER_MIN, CLEAR_WEATHER_MAX, 0x1234_5678);
                }
            }
            if thundering != self.thundering || raining != self.raining {
                self.thundering = thundering;
                self.raining = raining;
                events.push(if raining {
                    (GAME_EVENT_RAIN_STARTED, 0.0)
                } else {
                    (GAME_EVENT_RAIN_STOPPED, 0.0)
                });
            }
        }

        // 梯度插值（雨/雷 → 1，否则 → 0；每 tick 0.01，钳制 0..1）
        let prev_rain = self.rain_gradient;
        let prev_thunder = self.thunder_gradient;
        self.rain_gradient =
            (self.rain_gradient + (f32::from(self.raining) * 0.02 - 0.01)).clamp(0.0, 1.0);
        self.thunder_gradient =
            (self.thunder_gradient + (f32::from(self.thundering) * 0.02 - 0.01)).clamp(0.0, 1.0);
        if self.rain_gradient != prev_rain {
            events.push((GAME_EVENT_RAIN_GRADIENT, self.rain_gradient));
        }
        if self.thunder_gradient != prev_thunder {
            events.push((GAME_EVENT_THUNDER_GRADIENT, self.thunder_gradient));
        }
        events
    }
}

impl Default for WeatherState {
    fn default() -> Self {
        Self::new()
    }
}

/// `GAME_EVENT` 事件 ID（对照 `GameStateChangeS2CPacket.Reason`）。
pub const GAME_EVENT_RAIN_STARTED: u8 = 1;
pub const GAME_EVENT_RAIN_STOPPED: u8 = 2;
pub const GAME_EVENT_RAIN_GRADIENT: u8 = 7;
pub const GAME_EVENT_THUNDER_GRADIENT: u8 = 8;

#[cfg(test)]
mod tests {
    use super::{
        WeatherState, GAME_EVENT_RAIN_GRADIENT, GAME_EVENT_RAIN_STARTED,
        GAME_EVENT_RAIN_STOPPED, GAME_EVENT_THUNDER_GRADIENT,
    };

    #[test]
    fn gradient_rises_while_raining_and_falls_after() {
        let mut weather = WeatherState::new();
        assert!(!weather.is_raining());
        // 强制进入下雨（直接翻转状态再插值）：
        // 通过 tick 切换需要等待随机时长，这里手动构造一个已在雨中的状态。
        let mut raining = WeatherState::new();
        raining.raining = true;
        raining.thundering = true;
        let mut saw_rain_gradient = false;
        let mut saw_thunder_gradient = false;
        for _ in 0..100 {
            for (event, _) in raining.tick(true) {
                saw_rain_gradient |= event == GAME_EVENT_RAIN_GRADIENT;
                saw_thunder_gradient |= event == GAME_EVENT_THUNDER_GRADIENT;
            }
        }
        assert!(saw_rain_gradient, "rain gradient should change");
        assert!(saw_thunder_gradient, "thunder gradient should change");
        assert!(raining.rain_gradient() > 0.5);
        assert!(raining.thunder_gradient() > 0.5);

        // 雨后梯度回落
        let mut clearing = WeatherState::new();
        clearing.raining = true;
        clearing.rain_gradient = 1.0;
        clearing.thunder_gradient = 1.0;
        clearing.raining = false;
        clearing.thundering = false;
        for _ in 0..100 {
            let _ = clearing.tick(true);
        }
        assert!(clearing.rain_gradient() < 0.5);
        assert!(clearing.thunder_gradient() < 0.5);
    }

    #[test]
    fn cycle_toggles_rain_with_start_stop_events() {
        let mut weather = WeatherState::new();
        // 用固定种子强制：clear_time=0、rain_time=1 → 下一 tick 翻转进入下雨
        weather.clear_time = 0;
        weather.rain_time = 1;
        weather.thunder_time = 1;
        let mut started = false;
        for _ in 0..10 {
            for (event, _) in weather.tick(true) {
                started |= event == GAME_EVENT_RAIN_STARTED;
            }
        }
        assert!(started, "rain should start within a few ticks");
        // 再推进到雨停
        weather.clear_time = 0;
        weather.rain_time = 1;
        let mut stopped = false;
        for _ in 0..10 {
            for (event, _) in weather.tick(true) {
                stopped |= event == GAME_EVENT_RAIN_STOPPED;
            }
        }
        assert!(stopped, "rain should stop");
    }

    #[test]
    fn do_weather_cycle_false_freezes_state() {
        let mut weather = WeatherState::new();
        weather.raining = true;
        weather.rain_gradient = 0.5;
        let mut events: Vec<u8> = Vec::new();
        for _ in 0..50 {
            for (event, _) in weather.tick(false) {
                events.push(event);
            }
        }
        // 不推进切换：梯度继续插值（已在下雨则维持），但不得出现开始/停止
        assert!(!events.contains(&GAME_EVENT_RAIN_STARTED));
        assert!(!events.contains(&GAME_EVENT_RAIN_STOPPED));
        assert!(weather.is_raining());
        assert!(weather.rain_gradient() > 0.5);
    }
}
