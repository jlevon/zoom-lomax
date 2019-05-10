/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2019 John Levon <levon@movementarian.org>
 */

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path;
use std::process;

use chrono::{Datelike, DateTime, FixedOffset, Local, Utc};
use chrono_tz::Tz;
use dirs;
use reqwest;
use serde;
use serde_json;

#[derive(Debug)]
struct NoHomeDirError;

impl Error for NoHomeDirError {}

impl fmt::Display for NoHomeDirError {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "couldn't locate home directory")
	}
}

#[derive(serde::Deserialize, Debug)]
struct Config {
	api_key: String,
	api_secret: String,
	// FIXME: email address - should we parse as such?
	user: String,
	output_dir: String
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

fn read_config_file() -> Result<Config, Box<Error>> {
	let home = match dirs::home_dir() {
		Some(path) => path,
		None => return Err(Box::new(NoHomeDirError))
	};

	let mut cfg_file = home;
        cfg_file.push(".zoom-lomax");

	let file = fs::File::open(cfg_file)?;
	let config = serde_json::from_reader(file)?;

	Ok(config)
}

fn get_user(client: &reqwest::Client, config: &Config)
    -> Result<ZoomUser, Box<Error>> {
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
    host_id: &String) -> Result<ZoomMeetings, Box<Error>> {

	let params = [
	    ("api_key", &config.api_key as &str),
	    ("api_secret", &config.api_secret as &str),
	    ("email", &config.user as &str),
	    ("host_id", host_id as &str),
	    ("page_size", "10"),
	    ("data_type", "JSON")
	];

	let mut res = client.post("https://api.zoom.us/v1/recording/list")
	    .form(&params)
	    .send()?;

	Ok(res.json()?)
}

fn is_today<D: Datelike>(mtime: &D, tz: &Tz) -> bool {
	let lnow = Utc::now().with_timezone(tz);

	mtime.year() == lnow.year() &&
	    mtime.month() == lnow.month() &&
	    mtime.day() == lnow.day()
}

fn create_meeting_dir(config: &Config, mtime: &DateTime<FixedOffset>) ->
    std::io::Result<path::PathBuf> {
	let mut dir = path::PathBuf::from(&config.output_dir);
	dir.push(mtime.format("%Y-%m-%d").to_string());

	fs::create_dir_all(&dir)?;
	Ok(dir)
}

fn download(client: &reqwest::Client, url: &str,
    outfile: &path::PathBuf) -> Result<(), Box<Error>> {
	let mut out = fs::File::create(outfile)?;
	let mut resp = client.get(url).send()?;

	io::copy(&mut resp, &mut out)?;
	Ok(())
}

fn download_meeting(client: &reqwest::Client, config: &Config,
    meeting: &ZoomMeeting, mtime: &DateTime<FixedOffset>,
    tz: &Tz) -> () {

	let dir = create_meeting_dir(&config, &mtime).unwrap();

	let ltime = mtime.with_timezone(tz);

	for recording in &meeting.recording_files {
		let suffix = ".".to_string() +
		    &recording.file_type.to_ascii_lowercase();
		let mut outfile = dir.clone();
		outfile.push(ltime.format("%H.%M").to_string() +
		    &suffix);

		if outfile.exists() {
			continue;
		}

		println!("Downloading {} file for meeting at {}",
		    suffix, ltime);

		download(&client, &recording.download_url,
		    &outfile).unwrap();
	}
}

fn run(config: &Config) -> Result<(), Box<Error>> {
	let now = Local::now();

	println!("{}: downloading meetings for user {} to {}",
	    now.format("%Y-%m-%d"), config.user, config.output_dir);

	let client = reqwest::Client::new();

	let user = get_user(&client, &config)?;

	let meetings = get_meetings(&client, &config, &user.id)?;

	for meeting in meetings.meetings {
		let tz: Tz = meeting.timezone.parse()?;
		let mtime = DateTime::parse_from_rfc3339(
		    &meeting.start_time)?;

		if !is_today(&mtime, &tz) {
			continue;
		}

		download_meeting(&client, &config, &meeting,
		    &mtime, &tz);
	}

	Ok(())
}

fn main() {

	let config = match read_config_file() {
		Ok(config) => config,
		Err(err) => {
			eprintln!("Failed to read config file: {}",
			    err);
			process::exit(1);
		}
	};

	if let Err(err) = run(&config) {
		eprintln!("{}", err);
		process::exit(1);
	}
}
