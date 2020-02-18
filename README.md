# zoom-lomax: download Zoom recordings

This is a very simple utility, designed to be run from cron daily,
which will collect "field recordings" from your Zoom acccount.

It will download any recordings found in the last "days" days, as
calculated with the meeting's local timezone.

To use, populate ~/.zoom-lomax like this:

    {
        "api_key": "sdk_api_key",
        "api_secret": "sdk_api_secret",
        "user": "zoom_user_email@example.com",
        "output_dir": "/home/user/zoom-recordings/",
        "days": 1,
        "notify": "Your Name <you@example.com>"
    }

`days` and `notify` are optional, with the former defaulting to 1 day.

(You can also use the `-c` command-line option to specify a different
config file.)

They don't make it obvious, but you will need a Pro account in order to
be able to get the key and secret. The account owner has to follow the
steps described here:

https://marketplace.zoom.us/docs/sdk/native-sdks/preface/sdk-keys-secrets

under "Create new SDK credentials".

## Building and running

Written in Rust; after installing Rust with `rustup`, build with:

```
$ cargo build
```

and run with:

```
$ ./target/debug/zoom-lomax
```

## AWS Lambda support

This can also run as a lambda function. In this case, no downloads are
done, and the notification email contents provides a list of URIs and
times. It's easiest to build this with
[https://github.com/emk/rust-musl-builder](rust-musl-builder). The
function also returns a payload listing all matching recordings and
their URLs.

The JSON configuration above should be provided as the event for the
Lambda handler, except that `api_key` and `api_secret` should
instead refer to Parameter Store names, each of which contain a
`SecureString` with the value. `output_dir` is ignored currently.

Email notification works via Amazon SES, so that must be configured
(domain and potentially destination email both verified). Right now it's
hard-coded to use the `us-east-1` region.

The Lambda function needs to run as a role with permissions for SES, and SSM
parameter store (such as `AmazonSESFullAccess` and `AmazonSSMReadOnlyAccess`).
