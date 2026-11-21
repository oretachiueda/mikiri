//! S002 development-period COT hedging-pressure search.

use jiff::civil::{Date, date};
use std::error::Error;

mod input;
mod signals;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const DEV_START: Date = date(1996, 1, 2);
const DEV_END: Date = date(2024, 10, 31);
