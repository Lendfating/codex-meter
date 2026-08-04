use time::{macros::format_description, OffsetDateTime, UtcOffset};

const SHANGHAI_DATE_FORMAT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]");

fn shanghai_offset() -> UtcOffset {
    UtcOffset::from_hms(8, 0, 0).expect("Asia/Shanghai has a fixed +08:00 offset")
}

pub fn utc_epoch_ms_to_shanghai(
    epoch_ms: i64,
) -> Result<OffsetDateTime, time::error::ComponentRange> {
    let utc = OffsetDateTime::from_unix_timestamp_nanos(i128::from(epoch_ms) * 1_000_000)?;
    Ok(utc.to_offset(shanghai_offset()))
}

pub fn shanghai_date(epoch_ms: i64) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    utc_epoch_ms_to_shanghai(epoch_ms)?
        .format(SHANGHAI_DATE_FORMAT)
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_displayed_in_asia_shanghai() {
        let local = utc_epoch_ms_to_shanghai(0).unwrap();
        assert_eq!(local.hour(), 8);
        assert_eq!(shanghai_date(0).unwrap(), "1970-01-01");
    }
}
