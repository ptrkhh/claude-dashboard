import express from 'express';

const app = express();
app.use(express.json());
app.use(express.static('public'));

app.get('/api/health', (_req, res) => res.json({ ok: true }));

const port = process.env.PORT || 8080;
app.listen(port, () => console.log(`claude-dashboard on http://localhost:${port}`));
