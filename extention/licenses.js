function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function formatLink(url) {
  if (!url) return '';
  const safe = escapeHtml(url);
  return `<a href="${safe}" target="_blank" rel="noreferrer noopener">${safe}</a>`;
}

function renderTable(items, columns) {
  if (!Array.isArray(items) || items.length === 0) {
    return '<p class="muted">No entries.</p>';
  }

  const head = columns.map((column) => `<th>${escapeHtml(column.label)}</th>`).join('');
  const body = items.map((item) => {
    const cells = columns.map((column) => {
      const value = item?.[column.key];
      if (column.type === 'link') {
        return `<td>${formatLink(value) || '-'}</td>`;
      }
      return `<td>${escapeHtml(value || '-')}</td>`;
    }).join('');
    return `<tr>${cells}</tr>`;
  }).join('');

  return `
    <div class="table-wrap">
      <table>
        <thead><tr>${head}</tr></thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderNotes(notes) {
  if (!Array.isArray(notes) || notes.length === 0) {
    return '<p class="muted">No additional notes.</p>';
  }
  const rows = notes.map((note) => `<li>${escapeHtml(note)}</li>`).join('');
  return `<ul>${rows}</ul>`;
}

function setStatus(message) {
  document.getElementById('status').textContent = message;
}

async function loadAndRender() {
  try {
    const response = await fetch('licenses-third-party.json', { cache: 'no-store' });
    if (!response.ok) {
      throw new Error(`Failed to load licenses-third-party.json: ${response.status}`);
    }

    const data = await response.json();

    const generatedAt = data.generatedAtUtc ? new Date(data.generatedAtUtc).toLocaleString() : 'N/A';
    setStatus(`Generated: ${generatedAt}`);

    const project = data.project || {};
    document.getElementById('project-license').innerHTML = `
      <p>
        <strong>${escapeHtml(project.name || 'browser-port')}</strong><br>
        License: <code>${escapeHtml(project.license || 'UNKNOWN')}</code><br>
        License URL: ${formatLink(project.licenseTextUrl) || '-'}<br>
        Repository license file: ${escapeHtml(project.licenseFile || '-')}
      </p>
    `;

    document.getElementById('rust-crates').innerHTML = renderTable(
      data.rustCrates || [],
      [
        { key: 'name', label: 'Name' },
        { key: 'version', label: 'Version' },
        { key: 'license', label: 'License' },
        { key: 'repository', label: 'Repository', type: 'link' },
      ],
    );

    document.getElementById('npm-packages').innerHTML = renderTable(
      data.extensionNpmPackages || [],
      [
        { key: 'name', label: 'Name' },
        { key: 'version', label: 'Version' },
        { key: 'license', label: 'License' },
      ],
    );

    document.getElementById('native-libraries').innerHTML = renderTable(
      data.nativeBundledLibraries || [],
      [
        { key: 'name', label: 'Name' },
        { key: 'license', label: 'License' },
        { key: 'licenseFile', label: 'License File Path' },
        { key: 'homepage', label: 'Homepage', type: 'link' },
      ],
    );

    document.getElementById('notes').innerHTML = renderNotes(data.notices || []);
  } catch (error) {
    setStatus('Failed to load license data.');
    document.getElementById('notes').innerHTML = `<p>${escapeHtml(error.message || String(error))}</p>`;
  }
}

loadAndRender();
