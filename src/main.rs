/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2019 John Levon <levon@movementarian.org>
 */

//! # zoom-lomax: download recordings from Zoom.
//!
//! See [github](https://github.com/jlevon/zoom-lomax/) for details.

use std::fs;
use std::io;
use std::path;
use std::process;
use std::str::FromStr;

use chrono::{DateTime, Duration, Local, Timelike, Utc};
use chrono_tz::Tz;
use clap;
use dirs;
use env_logger;
use failure::{err_msg, Error, Fail};
use lettre::{SendmailTransport, Transport};
use lettre_email::{EmailBuilder, Mailbox};
use log::debug;
use reqwest;
use serde;
use serde_json;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Fail, Debug)]
#[fail(display = "couldn't locate home directory")]
struct NoHomeDirError;

/*
 * This only exists because we're not allowed to impl Deserialize for lettre_email::Mailbox.
 */
#[derive(Debug)]
struct EmailAddress {
    mailbox: Mailbox,
}

fn default_days() -> i64 {
    1
}

#[derive(serde::Deserialize, Debug)]
struct Config {
    api_key: String,
    api_secret: String,
    // this has to be of the form foo@bar.com, hence we don't use Mailbox
    user: String,
    output_dir: String,
    #[serde(default = "default_days")]
    days: i64,
    notify: Option<EmailAddress>,
}

/*
 * These only contains the minimum we need.
 */

#[derive(serde::Deserialize, Debug)]
struct ZoomUser {
    id: String,
}

#[derive(serde::Deserialize, Debug)]
struct ZoomRecordingFile {
    file_type: String,
    download_url: String,
}

#[derive(serde::Deserialize, Debug)]
struct ZoomMeeting {
    start_time: String,
    timezone: String,
    recording_files: Vec<ZoomRecordingFile>,
}

#[derive(serde::Deserialize, Debug)]
struct ZoomMeetings {
    meetings: Vec<ZoomMeeting>,
}

impl<'de> serde::Deserialize<'de> for EmailAddress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<EmailAddress, D::Error> {
        let value = String::deserialize(deserializer)?;
        match Mailbox::from_str(&value) {
            Ok(m) => Ok(EmailAddress { mailbox: m }),
            Err(_) => Err(serde::de::Error::custom(format!(
                "Invalid email adddress {:?}",
                value
            ))),
        }
    }
}

fn get_default_config_file() -> Result<path::PathBuf, Error> {
    let home = dirs::home_dir().ok_or(NoHomeDirError)?;
    let mut cfg_file = home;
    cfg_file.push(".zoom-lomax");
    Ok(cfg_file)
}

fn read_config_file(cfg_file: &path::PathBuf) -> Result<Config, Error> {
    let file = fs::File::open(cfg_file)?;
    let config = serde_json::from_reader(file)?;

    debug!("Parsed config as {:?}\n", config);

    Ok(config)
}

fn get_user(client: &reqwest::Client, config: &Config) -> Result<ZoomUser, Error> {
    let params = [
        ("api_key", &config.api_key as &str),
        ("api_secret", &config.api_secret as &str),
        ("email", &config.user.to_string()),
        ("data_type", "JSON"),
    ];

    let mut res = client
        .post("https://api.zoom.us/v1/user/getbyemail")
        .form(&params)
        .send()?;

    let user = res.json()?;

    debug!("Got user data: {:#?}\n", user);

    Ok(user)
}

fn get_meetings(
    client: &reqwest::Client,
    config: &Config,
    host_id: &str,
) -> Result<ZoomMeetings, Error> {
    let params = [
        ("api_key", &config.api_key as &str),
        ("api_secret", &config.api_secret as &str),
        ("email", &config.user.to_string()),
        ("host_id", host_id as &str),
        ("page_size", "100"),
        ("data_type", "JSON"),
    ];

    let mut res = client
        .post("https://api.zoom.us/v1/recording/list")
        .form(&params)
        .send()?;

    let mlist = res.json()?;

    debug!("Got meeting list: {:#?}\n", mlist);

    Ok(mlist)
}

fn in_days_range(mtime: &DateTime<Tz>, days: i64) -> bool {
    let ctime = Utc::now().with_timezone(&mtime.timezone()) - Duration::days(days);

    *mtime > ctime
}

/*
 * Round a time like 09:58 to 10:00.
 */
fn round_time_to_hour(mtime: &mut DateTime<Tz>) {
    let minute = mtime.minute();
    const FUDGE: u32 = 5;
    let one_hour = Duration::hours(1);

    if minute >= 60 - FUDGE {
        mtime.clone_from(&(mtime.with_minute(0).unwrap().with_second(0).unwrap() + one_hour));
    } else if minute <= FUDGE {
        mtime.clone_from(&(mtime.with_minute(0).unwrap().with_second(0).unwrap()));
    }
}

fn create_meeting_dir(config: &Config, mtime: &DateTime<Tz>) -> io::Result<path::PathBuf> {
    let mut dir = path::PathBuf::from(&config.output_dir);
    dir.push(mtime.format("%Y-%m-%d").to_string());

    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn download(client: &reqwest::Client, url: &str, outfile: &path::PathBuf) -> Result<(), Error> {
    let mut out = fs::File::create(outfile)?;
    let mut resp = client.get(url).send()?;

    debug!("Downloading {}\n", url);
    io::copy(&mut resp, &mut out)?;
    debug!("Downloading {} completed\n", url);

    Ok(())
}

fn download_meeting(
    client: &reqwest::Client,
    config: &Config,
    mlist: &mut Vec<String>,
    meeting: &ZoomMeeting,
    mtime: &DateTime<Tz>,
) {
    let dir = create_meeting_dir(&config, &mtime).unwrap();

    for recording in &meeting.recording_files {
        let suffix = ".".to_string() + &recording.file_type.to_ascii_lowercase();
        let mut outfile = dir.clone();
        outfile.push(mtime.format("%H.%M").to_string() + &suffix);

        if outfile.exists() {
            continue;
        }

        println!("Downloading {} file for meeting at {}", suffix, mtime);

        mlist.push(outfile.to_string_lossy().to_string());

        download(&client, &recording.download_url, &outfile).unwrap();
    }
}

fn send_notification(recipient: &Mailbox, mlist: Vec<String>) {
    let now = Local::now().format("%Y-%m-%d");
    let subject = format!("{}: new Zoom recordings", now);

    let mut body = "New Zoom recordings are available:\n\n".to_owned();

    for file in mlist {
        body += &format!("{}\n", file);
    }

    let email = EmailBuilder::new()
        .to(recipient.clone())
        .from("zoom-lomax@movementarian.org")
        .subject(subject)
        .text(body)
        .build()
        .unwrap();

    debug!("Sending notification to {:?}\n", recipient);

    let result = SendmailTransport::new().send(email.into());

    if result.is_err() {
        eprintln!("Couldn't send email to {}: {:?}", recipient, result);
    }
}

fn run(matches: &clap::ArgMatches) -> Result<(), Error> {
    let config_file = matches
        .value_of("config")
        .map(path::PathBuf::from)
        .unwrap_or(get_default_config_file()?);

    debug!("using config file {}", config_file.display());

    let config = read_config_file(&config_file)?;

    let now = Local::now();
    let mut mlist = Vec::new();

    println!(
        "{}: downloading {}'s meetings for past {} days to {}",
        now.format("%Y-%m-%d"),
        config.user,
        config.days,
        config.output_dir
    );

    let client = reqwest::Client::new();
    let user = get_user(&client, &config)?;
    let meetings = get_meetings(&client, &config, &user.id)?;

    for meeting in meetings.meetings {
        /*
         * start_time is in UTC; we'll convert to local meeting
         * time here. Tz's FromStr has a String Err type, hence
         * the map_err().
         */
        let tz: Tz = meeting.timezone.parse().map_err(err_msg)?;

        let mut mtime = DateTime::parse_from_rfc3339(&meeting.start_time)?.with_timezone(&tz);

        debug!("Saw meeting {:#?}\n", meeting);

        if !in_days_range(&mtime, config.days) {
            continue;
        }

        round_time_to_hour(&mut mtime);

        download_meeting(&client, &config, &mut mlist, &meeting, &mtime);
    }

    if !mlist.is_empty() && config.notify.is_some() {
        send_notification(&config.notify.unwrap().mailbox, mlist);
    }

    Ok(())
}

fn main() {
    env_logger::init();

    let matches = clap::App::new("zoom-lomax")
        .version(VERSION)
        .about("Field recorder for Zoom")
        .arg(
            clap::Arg::with_name("config")
                .short("c")
                .long("config")
                .value_name("FILE")
                .help("Alternative config file")
                .takes_value(true),
        )
        .get_matches();

    if let Err(err) = run(&matches) {
        eprintln!("{}", err);
        process::exit(1);
    }
}

// vim: tabstop=4:shiftwidth=4:textwidth=100:expandtab
