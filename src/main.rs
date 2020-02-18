/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 John Levon <levon@movementarian.org>
 */

//! # zoom-lomax: download recordings from Zoom.
//!
//! See [github](https://github.com/jlevon/zoom-lomax/) for details.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path;
use std::process;
use std::str::FromStr;

use chrono::{DateTime, Duration, Local, Timelike, Utc};
use chrono_tz::Tz;
use dirs;
use env_logger;
use failure::{err_msg, Error, Fail};
use jsonwebtoken;
use lambda_runtime;
use lettre::{SendmailTransport, Transport};
use lettre_email::{EmailBuilder, Mailbox};
use log::debug;
use reqwest;
use rusoto_core;
use rusoto_ses::{Ses, SesClient};
use rusoto_ssm::{Ssm, SsmClient};
use serde;
use serde_json;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
struct Opt {
    /// Alternative config file
    #[structopt(name = "config", long, short, value_name = "FILE", parse(from_os_str))]
    config_file: Option<path::PathBuf>,
}

#[derive(Debug, Fail)]
#[fail(display = "couldn't locate home directory")]
struct NoHomeDirError;

/*
 * This only exists because we're not allowed to impl Deserialize for lettre_email::Mailbox.
 */
#[derive(Debug)]
struct EmailAddress(Mailbox);

fn default_days() -> i64 {
    1
}

// FIXME: shouldn't require output_dir for lambda mode

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

#[derive(serde::Serialize, Debug, Clone)]
struct RecordingFile {
    outfile: String,
    url: String,
}

#[derive(serde::Serialize, Debug, Clone)]
struct Recording {
    date: String,
    time: String,
    file: RecordingFile,
}

#[derive(serde::Serialize, Clone)]
struct Recordings {
    recordings: Vec<Recording>,
}

/// https://marketplace.zoom.us/docs/guides/authorization/jwt/jwt-with-zoom#requirements
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct JWTPayload {
    iss: String,
    exp: usize,
}

/*
 * The below only contain the minimum fields we need from the API-defined JSON.
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
            Ok(m) => Ok(EmailAddress(m)),
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

fn read_config(reader: impl io::Read) -> Result<Config, Error> {
    let config = serde_json::from_reader(reader)?;

    debug!("Parsed config as {:?}\n", config);

    Ok(config)
}

/// https://marketplace.zoom.us/docs/api-reference/zoom-api/cloud-recording/recordingslist
fn get_meetings(
    client: &reqwest::Client,
    config: &Config,
    headers: &reqwest::header::HeaderMap,
) -> Result<ZoomMeetings, Error> {
    let from = (Utc::now() - Duration::days(config.days))
        .format("%Y-%m-%d")
        .to_string();
    let to = Utc::now().format("%Y-%m-%d").to_string();

    let query = [("from", from), ("to", to)];

    let mut res = client
        .get(&format!(
            "https://api.zoom.us/v2/users/{}/recordings",
            config.user
        ))
        .headers(headers.clone())
        .query(&query)
        .send()?;

    let mlist = res.json()?;

    debug!("Got meeting list: {:#?}\n", mlist);

    Ok(mlist)
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

fn create_meeting_dir(config: &Config, date: &str) -> io::Result<path::PathBuf> {
    let mut dir = path::PathBuf::from(&config.output_dir);
    dir.push(date);

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

fn download_meetings(client: &reqwest::Client, config: &Config, rlist: &Recordings) {
    for recording in &rlist.recordings {
        let dir = create_meeting_dir(&config, &recording.date).unwrap();
        let mut outfile = dir;
        outfile.push(&recording.file.outfile);

        if outfile.exists() {
            continue;
        }

        println!(
            "Downloading recording file to {}",
            outfile.to_string_lossy()
        );
        download(&client, &recording.file.url, &outfile).unwrap();
    }
}

fn process_meeting(rlist: &mut Recordings, meeting: &ZoomMeeting, mtime: &DateTime<Tz>) {
    let date = mtime.format("%Y-%m-%d").to_string();

    for recording in &meeting.recording_files {
        let time = mtime.format("%H.%M").to_string();
        let outfile = time.clone() + "." + &recording.file_type.to_ascii_lowercase();

        let recording = Recording {
            time,
            date: date.clone(),
            file: RecordingFile {
                outfile,
                url: recording.download_url.clone(),
            },
        };

        debug!("Adding recording {:#?}\n", recording);

        rlist.recordings.push(recording);
    }
}

fn send_ses(recipient: &Mailbox, subject: &str, body: &str) {
    // FIXME: region hard-coded here as us-east-2 has no SES
    let sesclient = SesClient::new(rusoto_core::Region::UsEast1);
    let to = format!("{}", recipient);

    let result = sesclient
        .send_email(rusoto_ses::SendEmailRequest {
            destination: rusoto_ses::Destination {
                to_addresses: Some(vec![to]),
                ..rusoto_ses::Destination::default()
            },
            message: rusoto_ses::Message {
                subject: rusoto_ses::Content {
                    data: subject.to_string(),
                    ..rusoto_ses::Content::default()
                },
                body: rusoto_ses::Body {
                    text: Some(rusoto_ses::Content {
                        data: body.to_string(),
                        ..rusoto_ses::Content::default()
                    }),
                    ..rusoto_ses::Body::default()
                },
            },
            source: "zoom-lomax@movementarian.org".to_string(),
            ..rusoto_ses::SendEmailRequest::default()
        })
        .sync();

    if result.is_err() {
        eprintln!("Couldn't send email to {}: {:?}", recipient, result);
    }
}

fn send_notification(config: &Config, is_lambda: bool, rlist: &Recordings) {
    let now = Local::now().format("%Y-%m-%d");
    let subject = format!("{}: new Zoom recordings", now);
    let recipient = &config.notify.as_ref().unwrap().0;

    let mut body = "Zoom recordings are available:\n\n".to_owned();

    for recording in &rlist.recordings {
        if is_lambda {
            body += &format!(
                "{}/{}: {}\n",
                recording.date, recording.file.outfile, recording.file.url
            );
        } else {
            body += &format!(
                "{}/{}/{}\n",
                config.output_dir, recording.date, recording.file.outfile
            );
        }
    }

    debug!("Sending notification to {:?}\n", recipient);

    if is_lambda {
        send_ses(recipient, &subject, &body);
        return;
    }

    let email = EmailBuilder::new()
        .to(recipient.clone())
        .from("zoom-lomax@movementarian.org")
        .subject(subject)
        .text(body)
        .build()
        .unwrap();

    let result = SendmailTransport::new().send(email.into());

    if result.is_err() {
        eprintln!("Couldn't send email to {}: {:?}", recipient, result);
    }
}

fn run(config: Config, is_lambda: bool) -> Result<Recordings, Error> {
    let now = Local::now();
    let mut rlist = Recordings {
        recordings: Vec::new(),
    };

    println!(
        "{}: collecting {}'s meetings for past {} days",
        now.format("%Y-%m-%d"),
        config.user,
        config.days
    );

    let jwt_payload = JWTPayload {
        iss: config.api_key.to_string(),
        exp: (Utc::now() + Duration::minutes(30)).timestamp() as usize,
    };

    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &jwt_payload,
        config.api_secret.as_ref(),
    )?;

    debug!("JSON token: {:?} -> {}", jwt_payload, token);

    let mut headers = reqwest::header::HeaderMap::new();

    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", token).parse().unwrap(),
    );

    let client = reqwest::Client::new();

    let meetings = get_meetings(&client, &config, &headers)?;

    for meeting in meetings.meetings {
        /*
         * start_time is in UTC; we'll convert to local meeting
         * time here. Tz's FromStr has a String Err type, hence
         * the map_err().
         */
        let tz: Tz = meeting.timezone.parse().map_err(err_msg)?;

        let mut mtime = DateTime::parse_from_rfc3339(&meeting.start_time)?.with_timezone(&tz);

        debug!("Saw meeting {:#?}\n", meeting);

        round_time_to_hour(&mut mtime);

        process_meeting(&mut rlist, &meeting, &mtime);
    }

    if !is_lambda {
        download_meetings(&client, &config, &rlist);
    }

    if !rlist.recordings.is_empty() && config.notify.is_some() {
        send_notification(&config, is_lambda, &rlist);
    }

    Ok(rlist)
}

fn run_cmdline(opt: Opt) -> Result<(), Error> {
    let config_file = opt.config_file.unwrap_or(get_default_config_file()?);

    debug!("using config file {}", config_file.display());

    let config = read_config(fs::File::open(&config_file)?)?;

    run(config, false).map(|_r| ())
}

fn run_lambda(
    event: Config,
    _context: lambda_runtime::Context,
) -> Result<Recordings, lambda_runtime::error::HandlerError> {
    let ssmreq = rusoto_ssm::GetParametersRequest {
        names: vec![event.api_key.clone(), event.api_secret.clone()],
        with_decryption: Some(true),
    };

    let result = SsmClient::new(rusoto_core::Region::default())
        .get_parameters(ssmreq)
        .sync()
        .unwrap();

    /*
     * The parameter details are annoyingly wrapped in a bunch of Options, unwrap them all.
     */
    let params: HashMap<_, _> = result
        .parameters
        .unwrap()
        .into_iter()
        .map(|p| (p.name.unwrap(), p.value.unwrap()))
        .collect();

    let api_key = params
        .get(&event.api_key)
        .expect("api_key mising from parameter store")
        .to_string();
    let api_secret = params
        .get(&event.api_secret)
        .expect("api_secret mising from parameter store")
        .to_string();

    let config = Config {
        api_key,
        api_secret,
        ..event
    };
    run(config, true).map_err(|err| err.into())
}

fn main() {
    env_logger::init();

    if env::var_os("AWS_REGION").is_some() {
        lambda_runtime::lambda!(run_lambda);
        process::exit(0);
    }

    if let Err(err) = run_cmdline(Opt::from_args()) {
        eprintln!("{}", err);
        process::exit(1);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_invalid_config() {
        read_config("".as_bytes()).expect_err("should fail");
        read_config("foo".as_bytes()).expect_err("should fail");
        read_config("{".as_bytes()).expect_err("should fail");
        read_config("{}".as_bytes()).expect_err("should fail");
        read_config(
            r#"{
            "api_key": "key",
            "api_secret": "secret",
            "user": "user@example.com"
        }"#
            .as_bytes(),
        )
        .expect_err("should fail");
        read_config(
            r#"{
            "api_key": "key",
            "api_secret": "secret",
            "output_dir": "/home/me/dir",
            "user": "user@example.com",
            "notify": "<user@foo.com"
        }"#
            .as_bytes(),
        )
        .expect_err("should fail");
        read_config(
            r#"{
            "api_key": "key",
            "api_secret": "secret",
            "output_dir": "/home/me/dir",
            "user": "user@example.com",
            "days", "foo"
        }"#
            .as_bytes(),
        )
        .expect_err("should fail");
        /* FIXME: bug in rust-email
                read_config(r#"{
                    "api_key": "key",
                    "api_secret": "secret",
                    "output_dir": "/home/me/dir",
                    "user": "user@example.com",
                    "notify": "user@"
                }"#.as_bytes()).expect_err("should fail");
        */
    }

    #[test]
    fn test_valid_config() {
        read_config(
            r#"{
            "api_key": "key",
            "api_secret": "secret",
            "output_dir": "/home/me/dir",
            "user": "user@example.com"
        }"#
            .as_bytes(),
        )
        .expect("missing notify and days");
        read_config(
            r#"{
            "api_key": "key",
            "api_secret": "secret",
            "output_dir": "/home/me/dir",
            "user": "user@example.com",
            "notify": "My name <user@example.com>",
            "days": 4
        }"#
            .as_bytes(),
        )
        .expect("fully specified");
    }
}

// vim: tabstop=4:shiftwidth=4:textwidth=100:expandtab
