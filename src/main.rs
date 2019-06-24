/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2019 John Levon <levon@movementarian.org>
 */

#[macro_use] extern crate failure;

use std::fs;
use std::io;
use std::path;
use std::process;

use chrono::{DateTime, Duration, Local, Timelike, Utc};
use chrono_tz::Tz;
use clap;
use dirs;
use failure::{Error, err_msg};
use lettre::{Transport, SendmailTransport};
use lettre_email::EmailBuilder;
use reqwest;
use serde;
use serde_json;

const VERSION: &'static str = env!("CARGO_PKG_VERSION");

#[derive(Fail, Debug)]
#[fail(display = "couldn't locate home directory")]
struct NoHomeDirError;

#[derive(serde::Deserialize, Debug)]
struct Config {
	api_key: String,
	api_secret: String,
	// FIXME: email address - should we parse as such?
	user: String,
	output_dir: String,
	days: i64,
	// FIXME: email address - should we parse as such?
	 #[serde(default)]
	notify: String
}

/*
 * These only contains the minimum we need.
 */

#[derive(serde::Deserialize, Debug)]
struct ZoomUser {
	id: String
}

#[derive(serde::Deserialize, Debug)]
struct ZoomRecordingFile {
	file_type: String,
	download_url: String
}

#[derive(serde::Deserialize, Debug)]
struct ZoomMeeting {
	start_time: String,
	timezone: String,
	recording_files: Vec<ZoomRecordingFile>
}

#[derive(serde::Deserialize, Debug)]
struct ZoomMeetings {
	meetings: Vec<ZoomMeeting>
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

	Ok(config)
}

fn get_user(client: &reqwest::Client, config: &Config)
    -> Result<ZoomUser, Error> {
	let params = [
	    ("api_key", &config.api_key as &str),
	    ("api_secret", &config.api_secret as &str),
	    ("email", &config.user as &str),
	    ("data_type", "JSON")
	];

	let mut res = client.post("https://api.zoom.us/v1/user/getbyemail")
	    .form(&params)
	    .send()?;

	Ok(res.json()?)
}

fn get_meetings(client: &reqwest::Client, config: &Config,
    host_id: &String) -> Result<ZoomMeetings, Error> {

	let params = [
	    ("api_key", &config.api_key as &str),
	    ("api_secret", &config.api_secret as &str),
	    ("email", &config.user as &str),
	    ("host_id", host_id as &str),
	    ("page_size", "100"),
	    ("data_type", "JSON")
	];

	let mut res = client.post("https://api.zoom.us/v1/recording/list")
	    .form(&params)
	    .send()?;

	Ok(res.json()?)
}

fn in_days_range(mtime: &DateTime<Tz>, days: i64) -> bool {
	let ctime = Utc::now().with_timezone(&mtime.timezone())
	    - Duration::days(days);

	mtime > &ctime
}

/*
 * Round a time like 09:58 to 10:00.
 */
fn round_time_to_hour(mtime: &mut DateTime<Tz>) {
	let minute = mtime.minute();
	const FUDGE: u32 = 5;
	let one_hour = Duration::hours(1);

	if minute >= 60 - FUDGE {
		mtime.clone_from(&(mtime
		    .with_minute(0).unwrap()
		    .with_second(0).unwrap()
		    + one_hour));
	} else if minute <= FUDGE {
		mtime.clone_from(&(mtime
		    .with_minute(0).unwrap()
		    .with_second(0).unwrap()));
	}
}

fn create_meeting_dir(config: &Config, mtime: &DateTime<Tz>) ->
    std::io::Result<path::PathBuf> {
	let mut dir = path::PathBuf::from(&config.output_dir);
	dir.push(mtime.format("%Y-%m-%d").to_string());

	fs::create_dir_all(&dir)?;
	Ok(dir)
}

fn download(client: &reqwest::Client, url: &str,
    outfile: &path::PathBuf) -> Result<(), Error> {
	let mut out = fs::File::create(outfile)?;
	let mut resp = client.get(url).send()?;

	io::copy(&mut resp, &mut out)?;
	Ok(())
}

fn download_meeting(client: &reqwest::Client, config: &Config,
    mlist: &mut Vec<String>, meeting: &ZoomMeeting,
    mtime: &DateTime<Tz>) -> () {
	let dir = create_meeting_dir(&config, &mtime).unwrap();

	for recording in &meeting.recording_files {
		let suffix = ".".to_string() +
		    &recording.file_type.to_ascii_lowercase();
		let mut outfile = dir.clone();
		outfile.push(mtime.format("%H.%M").to_string() +
		    &suffix);

		if outfile.exists() {
			continue;
		}

		println!("Downloading {} file for meeting at {}",
		    suffix, mtime);

		mlist.push(outfile.to_string_lossy().to_string());

		download(&client, &recording.download_url,
		    &outfile).unwrap();
	}
}

fn send_notification(recipient: &str, mlist: Vec<String>) {
	let now = Local::now().format("%Y-%m-%d");
	let subject = format!("{}: new Zoom recordings", now);

	let mut body = "New Zoom recordings are available:\n\n"
	    .to_owned();

	for file in mlist {
		body += &format!("{}\n", file);
	}

	let email = EmailBuilder::new()
	    .to(recipient)
	    .from("zoom-lomax@movementarian.org")
	    .subject(subject)
	    .text(body)
	    .build()
	    .unwrap();

	let result = SendmailTransport::new()
	    .send(email.into());

	if !result.is_ok() {
		eprintln!("Couldn't send email to {}: {:?}",
		    recipient, result);
	}
}

fn run(matches: &clap::ArgMatches) -> Result<(), Error> {

	let config_file = matches.value_of("config")
	    .map(|s| path::PathBuf::from(s))
	    .unwrap_or(get_default_config_file()?);

	let config = read_config_file(&config_file)?;

	let now = Local::now();
	let mut mlist = Vec::new();

	println!("{}: downloading {}'s meetings for past {} days to {}",
	    now.format("%Y-%m-%d"), config.user, config.days,
	    config.output_dir);

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

		let mut mtime = DateTime::parse_from_rfc3339(
		    &meeting.start_time)?.with_timezone(&tz);

		if !in_days_range(&mtime, config.days) {
			continue;
		}

		round_time_to_hour(&mut mtime);

		download_meeting(&client, &config, &mut mlist,
		    &meeting, &mtime);
	}

	if mlist.len() > 0 && !config.notify.is_empty() {
		send_notification(&config.notify, mlist);
	}

	Ok(())
}

fn main() {

	let matches = clap::App::new("zoom-lomax")
	    .version(VERSION)
	    .about("Field recorder for Zoom")
	    .arg(clap::Arg::with_name("config")
	    .short("c")
	    .long("config")
	    .value_name("FILE")
	    .help("Alternative config file")
	    .takes_value(true))
	    .get_matches();

	if let Err(err) = run(&matches) {
		eprintln!("{}", err);
		process::exit(1);
	}
}
