const express = require("express");
const request = require("request");
const path = require("path");
const app = express();
const cookie = process.env.COOKIE;

app.use(express.static(path.join(__dirname, "public")));

app.get("/", (req, res) => {
  res.sendFile(path.join(__dirname, "public", "index.html"));
});

app.get("/key", (req, res) => {
  res.send("success");
});

app.get("/getLyrics/:trackId", (req, res) => {
  // First request: Retrieve access token and client info
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
        console.error(tokenErr);
        return res.status(500).send(tokenErr);
      }
      let tokenJson;
      try {
        tokenJson = JSON.parse(tokenBody);
      } catch (parseError) {
        return res.status(500).send("Error parsing token response");
      }
      const accessToken = tokenJson.accessToken;
      console.log(accessToken);

      // Second request: Get lyrics using the access token
      request.get(
        {
          url:
            process.env.LYRICS_BASE_URL +
            `${req.params.trackId}?format=json&vocalRemoval=false&market=from_token`,
          headers: {
            "app-platform": "WebPlayer",
            Authorization: `Bearer ${accessToken}`,
          },
        },
        (lyricsErr, lyricsResponse, lyricsBody) => {
          if (lyricsErr) {
            return res.status(500).send(lyricsErr);
          }
          console.log(lyricsResponse.body);
          res.header("Access-Control-Allow-Origin", "*");
          try {
            const lyricsJson = JSON.parse(lyricsResponse.body);
            res.send(JSON.stringify(lyricsJson, null, 2));
          } catch (parseError) {
            res.status(500).send("Error parsing lyrics response");
          }
        }
      );
    }
  );
});

app.get("/getLyricsByName/:musician/:track", (req, res) => {
  const clientId = process.env.CLIENT_ID;
  const clientSecret = process.env.CLIENT_SECRET;
  const encoded = Buffer.from(`${clientId}:${clientSecret}`).toString("base64");

  // First request: Obtain a search access token
  request.post(
    {
      url: process.env.SEARCH_TOKEN,
      headers: {
        Authorization: `Basic ${encoded}`,
        "Content-Type": "application/x-www-form-urlencoded",
      },
      form: {
        grant_type: "client_credentials",
      },
    },
    (tokenErr, tokenResponse, tokenBody) => {
      if (tokenErr) {
        return res.status(500).send(tokenErr);
      }
      let tokenJson;
      try {
        tokenJson = JSON.parse(tokenBody);
      } catch (parseError) {
        return res.status(500).send("Error parsing search token response");
      }
      const accessToken = tokenJson.access_token;

      // Build the search URL using URL encoding
      const searchUrl =
        process.env.SEARCH_URL +
        `${encodeURIComponent(
          req.params.musician
        )}%20track:${encodeURIComponent(req.params.track)}&type=track&limit=10`;

      // Second request: Search for the track by musician and track name
      request.get(
        {
          url: searchUrl,
          headers: {
            Authorization: `Bearer ${accessToken}`,
          },
        },
        (searchErr, searchResponse, searchBody) => {
          if (searchErr) {
            return res.status(500).send(searchErr);
          }
          let searchJson;
          try {
            searchJson = JSON.parse(searchBody);
          } catch (parseError) {
            return res.status(500).send("Error parsing search response");
          }

          if (!searchJson.tracks.items.length) {
            return res.status(404).send("No remix lyrics was found");
          }

          let filteredTracks;
          if (req.query.remix === "true") {
            filteredTracks = searchJson.tracks.items
              .filter((track) => track.name.toLowerCase().includes("remix"))
              .sort((a, b) => b.popularity - a.popularity);

            if (filteredTracks.length === 0) {
              filteredTracks = searchJson.tracks.items
                .filter((track) => !track.name.toLowerCase().includes("remix"))
                .sort((a, b) => b.popularity - a.popularity);
            }
          } else {
            filteredTracks = searchJson.tracks.items
              .filter((track) => !track.name.toLowerCase().includes("remix"))
              .sort((a, b) => b.popularity - a.popularity);
          }

          const realTrack = filteredTracks.shift();
          if (realTrack) {
            console.log(realTrack.id);
            res.header("Access-Control-Allow-Origin", "*");
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
