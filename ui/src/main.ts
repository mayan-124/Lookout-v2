import { mount } from "svelte";
import "./tokens.css";
import "./app.css";
import { loadAppearance } from "./look.svelte";
import App from "./App.svelte";

// The saved theme is applied before the first paint.
loadAppearance().finally(() => mount(App, { target: document.getElementById("app")! }));
