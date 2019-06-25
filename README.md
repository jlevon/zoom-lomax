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
        # optional
        "notify": "Your Name <you@example.com>"
    }

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
$ cargo run
```
