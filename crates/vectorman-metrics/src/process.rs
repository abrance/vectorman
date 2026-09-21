use std::time::Instant;

pub struct ProcessSampler {
    started: Instant,
    last_cpu_secs: std::sync::Mutex<Option<f64>>,
}

pub struct ProcessSample {
    pub cpu_secs: Option<f64>,
    pub rss_bytes: Option<f64>,
    pub uptime_secs: f64,
}

impl ProcessSampler {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            last_cpu_secs: std::sync::Mutex::new(None),
        }
    }

    pub fn sample(&self) -> ProcessSample {
        let uptime_secs = self.started.elapsed().as_secs_f64();
        let cpu_delta = match read_cpu_secs() {
            Some(now) => {
                let mut last = self.last_cpu_secs.lock().unwrap();
                let delta = match *last {
                    Some(prev) if now >= prev => Some(now - prev),
                    None => Some(now),
                    Some(_) => None,
                };
                *last = Some(now);
                delta
            }
            None => None,
        };
        ProcessSample {
            cpu_secs: cpu_delta,
            rss_bytes: read_rss_bytes(),
            uptime_secs,
        }
    }
}

fn read_cpu_secs() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    parse_stat_cpu_ticks(&stat, clk_tck()?)
}

fn clk_tck() -> Option<f64> {
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 {
        Some(v as f64)
    } else {
        None
    }
}

fn read_rss_bytes() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_vmrss(&status)
}

pub(crate) fn parse_stat_cpu_ticks(stat: &str, ticks: f64) -> Option<f64> {
    if ticks <= 0.0 {
        return None;
    }
    let rest = stat.rsplit_once(')')?.1.trim();
    let mut fields = rest.split_whitespace();
    let utime: f64 = fields.nth(11)?.parse().ok()?;
    let stime: f64 = fields.next()?.parse().ok()?;
    Some((utime + stime) / ticks)
}

pub(crate) fn parse_vmrss(status: &str) -> Option<f64> {
    for line in status.lines() {
        if let Some(rest) = line.trim().strip_prefix("VmRSS:") {
            let kb: f64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024.0);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stat_cpu_reads_utime_and_stime() {
        let stat = "1 (cat) R 0 1 1 34816 1 4194304 0 0 0 0 10 20 0 0 20 0 1 0 123";
        assert_eq!(parse_stat_cpu_ticks(stat, 100.0), Some(0.3));
    }

    #[test]
    fn parse_vmrss_reads_kb() {
        let status = "Name:\tcat\nVmRSS:\t  1234 kB\nVmSize:\t9999 kB\n";
        assert_eq!(parse_vmrss(status), Some(1234.0 * 1024.0));
    }
}
