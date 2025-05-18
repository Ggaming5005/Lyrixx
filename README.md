# 🎵 Lyrixx – Real-Time Spotify Lyrics API

**Lyrixx** is a Node.js-based REST API that fetches synchronized lyrics for Spotify tracks by leveraging Musixmatch data. It offers endpoints to retrieve lyrics using either the Spotify Track ID or the combination of artist and track name.

🔗 **Live Demo**: [https://lyrixxx.vercel.app](https://lyrixxx.vercel.app)

---

## 🚀 Features

* 🎧 Fetch lyrics using Spotify Track ID.
* 🎤 Retrieve lyrics by specifying artist and track name.
* 🔄 Optional support for fetching official remix versions.
* 📦 Lightweight and fast, suitable for integration into various applications.

---

## 📚 API Documentation

### 1. **Get Lyrics by Spotify Track ID**

**Endpoint:**

```
GET /getLyrics/{trackId}
```

**Description:**
Retrieve lyrics using the Spotify Track ID.

**Example Request:**

```
GET https://lyrixxx.vercel.app/getLyrics/123456789
```

**Example Response:**

```json
{
  "trackId": "123456789",
  "title": "Lucid Dreams",
  "artist": "Juice WRLD",
  "lyrics": "I still see your shadows in my room..."
}
```

---

### 2. **Get Lyrics by Artist & Track Name**

**Endpoint:**

```
GET /getLyricsByName/{artistName}/{trackName}?remix={true|false}
```

**Description:**
Retrieve lyrics by specifying the artist and track name. Optionally, set `remix=true` to fetch only official remix versions.

**Example Request (Original):**

```
GET https://lyrixxx.vercel.app/getLyricsByName/JuiceWRLD/LucidDreams
```

**Example Request (Remix):**

```
GET https://lyrixxx.vercel.app/getLyricsByName/JuiceWRLD/LucidDreams?remix=true
```

**Example Response:**

```json
{
  "trackName": "Lucid Dreams",
  "artist": "Juice WRLD",
  "isRemix": false,
  "lyrics": "I still see your shadows in my room..."
}
```

---

## 🛠️ Technologies Used

* **Node.js**: JavaScript runtime environment.
* **Express.js**: Web framework for Node.js.
* **Musixmatch API**: Source for synchronized lyrics data.
* **Spotify API**: For track identification and metadata.

---

## 📂 Project Structure

```
Lyrixx/
├── routes/
│   ├── getLyrics.js
│   └── getLyricsByName.js
├── utils/
│   └── fetchLyrics.js
├── app.js
├── package.json
└── README.md
```

---

## 📦 Installation & Setup

1. **Clone the repository:**

   ```bash
   git clone https://github.com/Ggaming5005/Lyrixx.git
   cd Lyrixx
   ```

2. **Install dependencies:**

   ```bash
   npm install
   ```

3. **Start the server:**

   ```bash
   node app.js
   ```

4. **Access the API:**
   Navigate to `http://localhost:3000` in your browser or API client.

---

## 📄 License

This project is licensed under the [MIT License](LICENSE).

---

## 🤝 Contributing

Contributions are welcome! Please open an issue or submit a pull request for any enhancements or bug fixes.

---

## 📬 Contact

For questions or support, feel free to reach out via Discord: [discord.com](https://discord.com/users/687322874100580368)

