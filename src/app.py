from fastapi import FastAPI

app = FastAPI(title="decision-sandbox")


@app.get("/")
def read_root():
    return {"Hello": "World"}
