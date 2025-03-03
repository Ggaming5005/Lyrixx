const express = require("express");
const axios = require("axios"); // using axios instead of the deprecated 'request'
const path = require("path");
const app = express();

// Serve static files from the 'public' folder
app.use(express.static(path.join(__dirname, "public")));

// Route to serve the documentation/index page
app.get("/", (req, res) => {
  res.sendFile(path.join(__dirname, "public", "index.html"));
});

// Dummy route to check server status
app.get("/key", (req, res) => {
  res.send("success");
});

// Endpoint to fetch lyrics for a given track ID
app.get("/getLyrics/:trackId", async (req, res) => {
  try {
    // First, obtain a token from your lyrics service
    const tokenResponse = await axios.get(process.env.TOKEN_URL, {
      headers: {
        Cookie: process.env.COOKIE,
        "User-Agent":
          "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36",
      },
    });
    const { accessToken } = tokenResponse.data;

    // Now call your lyrics API with the trackId
    const lyricsResponse = await axios.get(
      `${process.env.LYRICS_BASE_URL}${req.params.trackId}?format=json&vocalRemoval=false&market=from_token`,
      {
        headers: {
          "app-platform": "WebPlayer",
          Authorization: `Bearer ${accessToken}`,
        },
      }
    );
    res.header("Access-Control-Allow-Origin", "*");
    res.send(JSON.stringify(lyricsResponse.data, null, 2));
  } catch (error) {
    console.error(error);
    res.status(500).send(error.toString());
  }
});

// Endpoint to search for a track by musician and track name, then fetch lyrics
app.get("/getLyricsByName/:musician/:track", async (req, res) => {
  try {
    // Client credentials to get Spotify access token for search
    const clientId = process.env.CLIENT_ID;
    const clientSecret = process.env.CLIENT_SECRET;
    const encoded = Buffer.from(`${clientId}:${clientSecret}`).toString(
      "base64"
    );

    const tokenResponse = await axios.post(
      process.env.SEARCH_TOKEN,
      "grant_type=client_credentials",
      {
        headers: {
          Authorization: `Basic ${encoded}`,
          "Content-Type": "application/x-www-form-urlencoded",
        },
      }
    );
    const accessToken = tokenResponse.data.access_token;

    const searchUrl =
      process.env.SEARCH_URL +
      `${encodeURIComponent(req.params.musician)}%20track:${encodeURIComponent(
        req.params.track
      )}&type=track&limit=10`;

    const searchResponse = await axios.get(searchUrl, {
      headers: { Authorization: `Bearer ${accessToken}` },
    });

    const tracks = searchResponse.data.tracks.items;
    if (!tracks.length) {
      return res.status(404).send("No track found");
    }

    let filteredTracks;
    if (req.query.remix === "true") {
      filteredTracks = tracks.filter((track) =>
        track.name.toLowerCase().includes("remix")
      );
      if (!filteredTracks.length) {
        filteredTracks = tracks.filter(
          (track) => !track.name.toLowerCase().includes("remix")
        );
      }
    } else {
      filteredTracks = tracks.filter(
        (track) => !track.name.toLowerCase().includes("remix")
      );
    }

    const realTrack = filteredTracks.sort(
      (a, b) => b.popularity - a.popularity
    )[0];
    if (realTrack) {
      console.log("Found track ID:", realTrack.id);
      res.header("Access-Control-Allow-Origin", "*");
      res.redirect(`/getLyrics/${realTrack.id}`);
    } else {
      res.header("Access-Control-Allow-Origin", "*");
      res.status(404).send("No track found");
    }
  } catch (error) {
    console.error(error);
    res.status(500).send(error.toString());
  }
});

// NEW: Endpoint to fetch the currently playing track for the authenticated user
app.get("/currently-playing", async (req, res) => {
  // You must have implemented OAuth to set the user access token (e.g., in req.session or a cookie)
  const userAccessToken = req.session ? req.session.userAccessToken : null;
  if (!userAccessToken) {
    return res.status(401).send("User not authenticated");
  }
  try {
    const currentTrackResponse = await axios.get(
      "https://api.spotify.com/v1/me/player/currently-playing",
      { headers: { Authorization: `Bearer ${userAccessToken}` } }
    );
    const trackData = currentTrackResponse.data;
    if (!trackData || !trackData.item) {
      return res.status(404).send("No track currently playing");
    }
    const trackId = trackData.item.id;
    // Redirect to the lyrics endpoint using the currently playing track's ID
    res.redirect(`/getLyrics/${trackId}`);
  } catch (error) {
    console.error(error);
    res.status(500).send(error.toString());
  }
});

// Start the server
app.listen(3000, () => {
  console.log("Server started on port 3000");
});
