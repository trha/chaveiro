// Drives the spike app the way a user would, then writes what it saw into
// #result as JSON, for a headless browser's --dump-dom to read.
window.__errors = [];
window.addEventListener("error", (event) => window.__errors.push(String(event.message)));

const tick = () => new Promise((resolve) => setTimeout(resolve, 20));
const button = (text) => [...document.querySelectorAll("button")].find((b) => b.textContent === text);

async function drive() {
  await tick();
  button("Add").click();
  await tick();
  button("Add").click();
  await tick();

  const input = document.querySelector("input");
  input.focus();
  input.value = "ada";
  input.dispatchEvent(new Event("input"));
  await tick();

  // A pass after the caret moved must not move it.
  input.setSelectionRange(1, 1);
  button("Add").click();
  await tick();
  const caret = input.selectionStart;

  // A plain blur reaches the app.
  input.blur();
  await tick();

  // Removing the focused input fires blur inside the pass that removes it.
  let blurAtRemoval = false;
  input.addEventListener("blur", () => (blurAtRemoval = true));
  input.focus();
  button("Next").click();
  await tick();
  button("Done").click();
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 100));

  document.getElementById("result").textContent = JSON.stringify({
    labels: [...document.querySelectorAll("span")].map((span) => span.textContent),
    caret,
    blurAtRemoval,
    inputGone: !document.querySelector("input"),
    errors: window.__errors,
  });
}
