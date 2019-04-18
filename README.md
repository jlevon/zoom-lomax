# zoom-lomax: download daily recordings

This is a very simple utility, designed to be run from cron daily,
which will collect the day's "field recordings" from your Zoom acccount.

It's currently hard-coded to download any files recorded on the day it
runs (with respect to the timezone of the recording).

To use, populate ~/.zoom-lomax like this:

    {
        "api_key": "sdk_api_key",
        "api_secret": "sdk_api_secret",
        "user": "zoom_user_email@you.com",
        "output_dir": "/home/user/zoom-recordings/"
    }

They don't make it obvious, but you will need a Pro account in order to
be able to get the key and secret. The account owner has to follow the
steps described here:

https://marketplace.zoom.us/docs/sdk/native-sdks/preface/sdk-keys-secrets

under "Create new SDK credentials".
