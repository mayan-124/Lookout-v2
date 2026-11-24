import { mount } from "svelte";
import "./tokens.css";
import "./app.css";
import "./look.svelte";
import App from "./App.svelte";

mount(App, { target: document.getElementById("app")! });
