const express = require("express");
const request = require("request");
const path = require("path");
const app = express();

const cookie = process.env.COOKIE;

// Helper: Check required environment variables
const requiredEnv = [
  "CLIENT_ID",
  "CLIENT_SECRET",
  "TOKEN_URL",
  "LYRICS_BASE_URL",
  "SEARCH_URL",
];
const missing = requiredEnv.filter((envVar) => !process.env[envVar]);
if (missing.length) {
  console.error("Missing required environment variables:", missing.join(", "));
  process.exit(1);
}

// Middleware for setting CORS headers for all responses
app.use((req, res, next) => {
  res.header("Access-Control-Allow-Origin", "*");
  next();
});

// Serve static files from the 'public' folder
app.use(express.static(path.join(__dirname, "public")));

app.get("/", (req, res) => {
  res.sendFile(path.join(__dirname, "public", "index.html"));
});

app.get("/key", (req, res) => {
  return res.send("success");
});

/**
 * /getLyrics/:trackId
 * This endpoint uses a token (retrieved from process.env.TOKEN_URL) and then calls your custom lyrics service.
 */
app.get("/getLyrics/:trackId", (req, res) => {
  console.log(
    "Request received at /getLyrics for track ID:",
    req.params.trackId
  );

  // First request: Retrieve access token from your lyrics service (or fallback to Spotify token if needed)
  request.get(
    {
      url: process.env.TOKEN_URL,
      headers: {
        Cookie: cookie,
        "User-Agent":
          "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36",
      },
    },
    (tokenErr, tokenResponse, tokenBody) => {
      if (tokenErr) {
        console.error("Token request error:", tokenErr);
        return res.status(500).send(tokenErr);
      }
      let tokenJson;
      try {
        tokenJson = JSON.parse(tokenBody);
      } catch (parseError) {
        console.error("Error parsing token response:", parseError);
        return res.status(500).send("Error parsing token response");
      }
      const accessToken = tokenJson.accessToken;
      if (!accessToken) {
        console.error("Access token missing in token response");
        return res.status(500).send("Access token missing");
      }
      console.log("Access Token from lyrics service:", accessToken);

      // Second request: Get lyrics using the access token from your lyrics service
      const lyricsUrl =
        process.env.LYRICS_BASE_URL +
        `${req.params.trackId}?format=json&vocalRemoval=false&market=from_token`;
      console.log("Fetching lyrics from:", lyricsUrl);
      request.get(
        {
          url: lyricsUrl,
          headers: {
            "app-platform": "WebPlayer",
            Authorization: `Bearer ${accessToken}`,
          },
        },
        (lyricsErr, lyricsResponse, lyricsBody) => {
          if (lyricsErr) {
            console.error("Lyrics request error:", lyricsErr);
            return res.status(500).send(lyricsErr);
          }
          console.log("Lyrics Response Body:", lyricsResponse.body);
          try {
            const lyricsJson = JSON.parse(lyricsResponse.body);
            res.header("Access-Control-Allow-Origin", "*");
            res.send(JSON.stringify(lyricsJson, null, 2));
          } catch (parseError) {
            console.error("Error parsing lyrics response:", parseError);
            res.status(500).send("Error parsing lyrics response");
          }
        }
      );
    }
  );
});

/**
 * /getLyricsByName/:musician/:track
 * This endpoint uses Spotify's official API to search for a track using the musician and track name.
 * Once the track is found, it redirects to /getLyrics/:trackId.
 */
app.get("/getLyricsByName/:musician/:track", (req, res) => {
  console.log(
    "Request received at /getLyricsByName for musician:",
    req.params.musician,
    "track:",
    req.params.track
  );

  // Encode the client ID and secret for Spotify token retrieval
  const clientId = process.env.CLIENT_ID;
  const clientSecret = process.env.CLIENT_SECRET;
  const encoded = Buffer.from(`${clientId}:${clientSecret}`).toString("base64");

  // Get Spotify access token using client credentials flow
  request.post(
    {
      url: "https://accounts.spotify.com/api/token",
      headers: {
        Authorization: `Basic ${encoded}`,
        "Content-Type": "application/x-www-form-urlencoded",
      },
      form: { grant_type: "client_credentials" },
    },
    (tokenErr, tokenResponse, tokenBody) => {
      if (tokenErr) {
        console.error("Spotify token request error:", tokenErr);
        return res.status(500).send(tokenErr);
      }
      let tokenJson;
      try {
        tokenJson = JSON.parse(tokenBody);
      } catch (parseError) {
        console.error("Error parsing Spotify token response:", parseError);
        return res.status(500).send("Error parsing Spotify token response");
      }
      const accessToken = tokenJson.access_token;
      if (!accessToken) {
        console.error("Spotify access token missing");
        return res.status(500).send("Spotify access token missing");
      }
      console.log("Spotify Access Token:", accessToken);

      // Build the Spotify API search URL with the musician and track name, and set the limit to 10
      const searchUrl =
        process.env.SEARCH_URL +
        `${encodeURIComponent(
          req.params.musician
        )}%20track:${encodeURIComponent(req.params.track)}&type=track&limit=10`;
      console.log("Spotify Search URL:", searchUrl);

      // Make a GET request to the Spotify API search URL
      request.get(
        {
          url: searchUrl,
          headers: {
            Authorization: `Bearer ${accessToken}`,
          },
        },
        (searchErr, searchResponse, searchBody) => {
          if (searchErr) {
            console.error("Spotify search request error:", searchErr);
            return res.status(500).send(searchErr);
          }
          let json;
          try {
            json = JSON.parse(searchBody);
          } catch (parseError) {
            console.error("Error parsing Spotify search response:", parseError);
            return res
              .status(500)
              .send("Error parsing Spotify search response");
          }
          if (
            !json.tracks ||
            !json.tracks.items ||
            json.tracks.items.length === 0
          ) {
            return res.status(404).send("No remix lyrics was found");
          }

          // Filter tracks if remix parameter is set
          let filteredTracks;
          if (req.query.remix === "true") {
            filteredTracks = json.tracks.items
              .filter((track) => track.name.toLowerCase().includes("remix"))
              .sort((a, b) => b.popularity - a.popularity);
            if (filteredTracks.length === 0) {
              // Fall back to non-remix if none found
              filteredTracks = json.tracks.items
                .filter((track) => !track.name.toLowerCase().includes("remix"))
                .sort((a, b) => b.popularity - a.popularity);
            }
          } else {
            filteredTracks = json.tracks.items
              .filter((track) => !track.name.toLowerCase().includes("remix"))
              .sort((a, b) => b.popularity - a.popularity);
          }

          const realTrack = filteredTracks.shift();
          if (realTrack) {
            console.log("Real Track ID:", realTrack.id);
            res.header("Access-Control-Allow-Origin", "*");
            // Redirect to /getLyrics/:trackId to fetch lyrics from your custom service
            res.redirect(`/getLyrics/${realTrack.id}`);
          } else {
            res.header("Access-Control-Allow-Origin", "*");
            res.status(404).send("No Remix lyrics was found");
          }
        }
      );
    }
  );
});

module.exports = app;

app.listen(3000, () => {
  console.log("Server started on port 3000");
});
