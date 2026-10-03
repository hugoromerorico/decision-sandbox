from workers import asgi

from app import app

Default = asgi.entrypoint(app)
