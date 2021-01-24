use actix_files as fs;
use actix_web::{middleware, web, App, HttpRequest, HttpResponse, HttpServer};
use serde::{Deserialize, Serialize};

use chrono::prelude::*;
use std::cmp::{max, min};
use std::collections::VecDeque;
use std::sync::RwLock;

mod date_serde {
    use chrono::prelude::*;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    const FORMAT: &str = "%Y-%m-%d";

    pub fn serialize<S: Serializer>(date: &Date<Local>, serializer: S) -> Result<S::Ok, S::Error> {
        format!("{}", date.format(FORMAT)).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Date<Local>, D::Error> {
        let s = String::deserialize(deserializer)?;
        Local
            .datetime_from_str(&s, FORMAT)
            .map_err(serde::de::Error::custom)
            .map(|dt| dt.date())
    }
}

// A power reading from a single machine
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
struct PowerReading {
    min: i16,
    max: i16,
    timestamp: DateTime<Local>,
}

impl PowerReading {
    pub fn new(min: i16, max: i16) -> PowerReading {
        PowerReading {
            min,
            max,
            timestamp: Local::now(),
        }
    }

    pub fn at_time(timestamp: DateTime<Local>, min: i16, max: i16) -> PowerReading {
        PowerReading {
            min,
            max,
            timestamp,
        }
    }

    pub fn difference(&self) -> i16 {
        self.max - self.min
    }
}

const HISTORY_SIZE: usize = 9;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct DayStats {
    num_readings: usize,
    mean: f64,
    min: i16,
    max: i16,

    #[serde(with = "date_serde")] // Custom module in this file
    day: Date<Local>,
}

impl DayStats {
    fn new(day: Date<Local>) -> DayStats {
        DayStats {
            num_readings: 0,
            mean: 0f64,
            min: 200i16,
            max: 1600i16,
            day,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChannelPowerHistory {
    readings: VecDeque<PowerReading>,
    today_stats: DayStats,
    yesterday_stats: Option<DayStats>,
}

impl ChannelPowerHistory {
    fn new() -> ChannelPowerHistory {
        let mut readings = VecDeque::with_capacity(HISTORY_SIZE);
        let timestamp = Local::now();
        for _ in 0..HISTORY_SIZE {
            readings.push_back(PowerReading::at_time(timestamp, 0, 0));
        }
        ChannelPowerHistory {
            readings,
            today_stats: DayStats::new(timestamp.date()),
            yesterday_stats: None,
        }
    }

    fn average_reading(&self) -> PowerReading {
        let num_readings = self.readings.len();
        let timestamp = self
            .readings
            .back()
            .expect("Cannot calculate average reading for an empty history")
            .timestamp;

        let min_sum: i16 = self.readings.iter().map(|reading| reading.min).sum();
        let min_mean = (min_sum as f64 / num_readings as f64) as i16;

        let max_sum: i16 = self.readings.iter().map(|reading| reading.max).sum();
        let max_mean = (max_sum as f64 / num_readings as f64) as i16;

        PowerReading::at_time(timestamp, min_mean, max_mean)
    }

    fn median_reading(&self) -> PowerReading {
        let timestamp = self
            .readings
            .back()
            .expect("Cannot calculate average reading for an empty history")
            .timestamp;
        let num_readings = self.readings.len();
        let mut sorted: Vec<PowerReading> = Vec::with_capacity(num_readings);
        sorted.extend(self.readings.iter());
        sorted.sort_unstable_by(|a, b| a.difference().cmp(&b.difference()));
        let median: PowerReading = sorted[num_readings / 2];
        PowerReading::at_time(timestamp, median.min, median.max)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct PowerHistory {
    a0: ChannelPowerHistory,
    a1: ChannelPowerHistory,
    a2: ChannelPowerHistory,
    a3: ChannelPowerHistory,
}

impl PowerHistory {
    pub fn new() -> PowerHistory {
        PowerHistory {
            a0: ChannelPowerHistory::new(),
            a1: ChannelPowerHistory::new(),
            a2: ChannelPowerHistory::new(),
            a3: ChannelPowerHistory::new(),
        }
    }

    pub fn update(&mut self, readings: &PowerReadings) {
        let PowerHistory {
            ref mut a0,
            ref mut a1,
            ref mut a2,
            ref mut a3,
        } = *self;

        let today = Local::today();

        for (channel_history, reading) in &mut [
            (a0, readings.a0),
            (a1, readings.a1),
            (a2, readings.a2),
            (a3, readings.a3),
        ] {
            let _ = channel_history.readings.pop_front();
            channel_history.readings.push_back(*reading);

            let range = reading.difference();
            let stats = &mut channel_history.today_stats;
            stats.mean = ((stats.num_readings as f64 * stats.mean) + (range as f64))
                 / (stats.num_readings + 1) as f64;
            stats.num_readings += 1;
            stats.max = max(stats.max, range);
            stats.min = min(stats.min, range);

            if stats.day != today {
                // It's a new day.
                channel_history.yesterday_stats = Some(stats.clone());

                // Originally, we would completely reset all stats. (mean=0, min=u16::MAX, max=u16::MIN)
                // This wasn't very helpful as it wrecked the front end each time the day rolled over.
                // Instead, we carry over the min, max, and mean and drop the num_readings to a low number
                // so it can readjust as the day progresses.
                stats.num_readings = 12 * 5; // Roughly 5 minutes' worth of readings
                stats.day = today;
            }
        }
    }
}

// Readings from all monitored machines
#[derive(Serialize, Deserialize, Clone, Debug)]
struct PowerReadings {
    a0: PowerReading,
    a1: PowerReading,
    a2: PowerReading,
    a3: PowerReading,
}

// Response to API requests
#[derive(Serialize, Deserialize, Clone, Debug)]
struct PowerResponse {
    history: PowerHistory,
    a0: PowerReading,
    a1: PowerReading,
    a2: PowerReading,
    a3: PowerReading,
}

impl PowerResponse {
    pub fn new(history: PowerHistory) -> PowerResponse {
        PowerResponse {
            history: history.clone(),
            a0: history.a0.median_reading(),
            a1: history.a1.median_reading(),
            a2: history.a2.median_reading(),
            a3: history.a3.median_reading(),
        }
    }
}

fn get_latest_readings(data: web::Data<RwLock<PowerHistory>>, _req: HttpRequest) -> HttpResponse {
    let power_history = data.read().unwrap();
    let power_response = PowerResponse::new(power_history.clone());
    HttpResponse::Ok().json(power_response)
}

fn set_latest_readings(
    data: web::Data<RwLock<PowerHistory>>,
    new_readings: web::Json<PowerReadings>,
    _req: HttpRequest,
) -> HttpResponse {
    let mut power_history = data.write().unwrap();
    power_history.update(&new_readings);
    HttpResponse::Ok().body("Updated.")
}

fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "actix_web=info");
    env_logger::init();

    let shared_power_readings = web::Data::new(RwLock::new(PowerHistory::new()));

    HttpServer::new(move || {
        App::new()
            .service(fs::Files::new("/laundry", "./www").index_file("index.html"))
            .wrap(middleware::Logger::default())
            .register_data(shared_power_readings.clone())
            .route("/power", web::get().to(get_latest_readings))
            .route("/power", web::post().to(set_latest_readings))
    })
    .bind("0.0.0.0:80")?
    .bind("localhost:80")?
    .run()
}
