//! The new tab page's weather widget. Forecasts come from Open-Meteo (free,
//! no account, no key); only the chosen city's coordinates are sent, and only
//! while the widget is on. The forecast is refreshed every 30 minutes while a
//! new tab page is open, and cached in between.

use super::{on_main, pages, with, Browser};
use volant_core::json;

const FRESH: u64 = 30 * 60;
/// After a failed fetch, wait this long before trying again.
const RETRY: u64 = 5 * 60;

#[derive(Default)]
pub struct Cache {
    at: u64,
    key: String,
    raw: String,
    /// When the last request went out.
    asked: u64,
}

/// Called every minute: keeps the forecast on open new tab pages current.
pub fn tick(b: &mut Browser) {
    if !b.settings.weather || cached(b).is_some() || super::now().saturating_sub(b.weather.asked) < RETRY {
        return;
    }
    let showing = b.tabs.list.iter().any(|t| t.url.starts_with(super::actions::NEWTAB) && b.webs.contains_key(&t.id));
    if showing {
        refresh(b);
    }
}

fn key(b: &Browser) -> Option<String> {
    let (lat, lon) = b.settings.weather_at?;
    Some(format!("{lat:.3},{lon:.3},{}", b.settings.weather_unit))
}

/// The last forecast, if it's still fresh, for drawing the page instantly.
pub fn cached(b: &Browser) -> Option<&str> {
    let k = key(b)?;
    (b.weather.key == k && super::now().saturating_sub(b.weather.at) < FRESH).then_some(b.weather.raw.as_str())
}

/// Fetches text over HTTPS on a background thread.
fn get(url: String, done: impl FnOnce(Option<String>) + Send + 'static) {
    std::thread::spawn(move || {
        let text = super::net::fetch_text(&url, 1 << 20).ok();
        on_main(move || done(text));
    });
}

/// Sends the forecast to the new tab pages, fetching it if the cache is stale.
pub fn refresh(b: &mut Browser) {
    if !b.settings.weather {
        return;
    }
    if let Some(raw) = cached(b) {
        let js = format!("window.volantWeather && volantWeather({})", json::string(raw));
        pages::notify(b, &js);
        return;
    }
    let (Some((lat, lon)), Some(k)) = (b.settings.weather_at, key(b)) else { return };
    b.weather.asked = super::now();
    let unit = if b.settings.weather_unit == "f" { "&temperature_unit=fahrenheit" } else { "" };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code,is_day&daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1{unit}"
    );
    get(url, move |text| {
        with(move |b| {
            match text.filter(|t| t.contains("\"current\"")) {
                Some(raw) => {
                    b.weather = Cache { at: super::now(), key: k, raw: raw.clone(), asked: b.weather.asked };
                    pages::notify(b, &format!("window.volantWeather && volantWeather({})", json::string(&raw)));
                }
                None => pages::notify(b, "window.volantWeather && volantWeather('', 'Couldn’t get the weather right now.')"),
            }
        })
    });
}

/// Looks a city up; the page picks the best match and saves it.
pub fn find_place(city: &str) {
    let url = format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json", volant_core::search::encode_query(city));
    get(url, move |text| {
        with(move |b| {
            let raw = text.unwrap_or_default();
            pages::notify(b, &format!("window.volantGeo && volantGeo({})", json::string(&raw)));
        })
    });
}
