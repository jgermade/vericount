// Fixture for the server tests.
const status = document.getElementById('status');

try {
  const response = await fetch('/healthz');
  status.textContent = response.ok ? 'Server is up.' : `Server error: ${response.status}`;
} catch {
  status.textContent = 'Server unreachable.';
}
