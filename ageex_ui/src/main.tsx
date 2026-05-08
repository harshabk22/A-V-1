import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import CommandBar from "./components/CommandBar";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {window.location.pathname === "/command-bar" ? <CommandBar /> : <App />}
  </React.StrictMode>,
);
