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
  request.get(
    {
      url: process.env.TOKEN_URL,
      headers: {
        Cookie: cookie,
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
      },
    },
    (error, response, body) => {
      if (error) {
        console.error("Error fetching token:", error);
        return res.status(500).send(error);
      }
      let tokenData;
      try {
        tokenData = JSON.parse(body);
      } catch (e) {
        console.error("Error parsing token response:", e);
        return res.status(500).send("Invalid token response");
      }
      const accessToken = tokenData.accessToken;
      console.log("Access Token:", accessToken);

      request.get(
        {
          url:
            process.env.LYRICS_BASE_URL +
            `/${req.params.trackId}?format=json&vocalRemoval=false&market=from_token`,
          headers: {
            "app-platform": "WebPlayer",
            Authorization: `Bearer ${accessToken}`,
          },
        },
        (error, response, body) => {
          if (error) {
            console.error("Error fetching lyrics:", error);
            return res.status(500).send(error);
          }
          res.header("Access-Control-Allow-Origin", "*");
          try {
            const lyricsData = JSON.parse(body);
            res.send(JSON.stringify(lyricsData, null, 2));
          } catch (e) {
            console.error("Error parsing lyrics response:", e);
            return res.status(500).send("Invalid lyrics response");
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
    (error, response, body) => {
      if (error) {
        console.error("Error getting search token:", error);
        return res.status(500).send(error);
      }
      let tokenResponse;
      try {
        tokenResponse = JSON.parse(body);
      } catch (e) {
        console.error("Error parsing search token response:", e);
        return res.status(500).send("Invalid search token response");
      }
      const accessToken = tokenResponse.access_token;

      const searchUrl =
        process.env.SEARCH_URL +
        `${encodeURIComponent(
          req.params.musician
        )}%20track:${encodeURIComponent(req.params.track)}&type=track&limit=10`;

      request.get(
        {
          url: searchUrl,
          headers: {
            Authorization: `Bearer ${accessToken}`,
          },
        },
        (error, response, body) => {
          if (error) {
            console.error("Error searching track:", error);
            return res.status(500).send(error);
          }
          let searchResult;
          try {
            searchResult = JSON.parse(body);
          } catch (e) {
            console.error("Error parsing search response:", e);
            return res.status(500).send("Invalid search response");
          }

          if (!searchResult.tracks || !searchResult.tracks.items.length) {
            return res
              .status(404)
              .send("No lyrics found for the provided track");
          }

          let tracks = searchResult.tracks.items;
          if (req.query.remix === "true") {
            tracks = tracks
              .filter((track) => track.name.toLowerCase().includes("remix"))
              .sort((a, b) => b.popularity - a.popularity);
            if (tracks.length === 0) {
              tracks = searchResult.tracks.items
                .filter((track) => !track.name.toLowerCase().includes("remix"))
                .sort((a, b) => b.popularity - a.popularity);
            }
          } else {
            tracks = tracks
              .filter((track) => !track.name.toLowerCase().includes("remix"))
              .sort((a, b) => b.popularity - a.popularity);
          }

          const realTrack = tracks[0];
          if (realTrack) {
            console.log("Selected track ID:", realTrack.id);
            res.header("Access-Control-Allow-Origin", "*");
            res.redirect(`/getLyrics/${realTrack.id}`);
          } else {
            res.header("Access-Control-Allow-Origin", "*");
            res.status(404).send("No lyrics were found");
          }
        }
      );
    }
  );
});

const PORT = process.env.PORT || 3000;
app.listen(PORT, () => {
  console.log(`Server started on port ${PORT}`);
});
