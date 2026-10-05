// Sets the theme before the first paint (a classic script, so it runs before
// the page is drawn): ?theme=dark|light, otherwise the system setting.
// main.js keeps following the system when it changes.
(function () {
  var theme = new URLSearchParams(window.location.search).get('theme');
  if (theme !== 'dark' && theme !== 'light') {
    theme = window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  document.documentElement.setAttribute('data-theme', theme);
})();
