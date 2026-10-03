"""Request and response models matching https://api.typesafe.ai/openapi.json.

Field names, types and descriptions follow the public TypeSafe contract. The only
additions are the sandbox's size limits (max lengths and item counts).
"""

from typing import Annotated, Any, Literal

from pydantic import BaseModel, Field, StringConstraints

from .limits import (
    MAX_CHOICE_NAME_LENGTH,
    MAX_CHOICES,
    MAX_QUESTION_NAME_LENGTH,
    MAX_QUESTIONS,
    MAX_SCORE_LEVELS,
)

# A free-form JSON value used for instructions and criteria descriptions.
Instructions = str | dict[str, Any] | list[Any] | None
Description = str | dict[str, Any] | list[Any]

QuestionName = Annotated[str, StringConstraints(max_length=MAX_QUESTION_NAME_LENGTH)]
ChoiceName = Annotated[str, StringConstraints(max_length=MAX_CHOICE_NAME_LENGTH)]


# --- Questions ---------------------------------------------------------------


class NoulCriteria(BaseModel):
    """Criteria defining what counts as a yes or no answer."""

    true: Instructions = Field(
        default=None,
        title="True",
        description="What counts as a yes answer.",
        examples=["The message is unsolicited advertising."],
    )
    false: Instructions = Field(
        default=None,
        title="False",
        description="What counts as a no answer.",
        examples=["The message is a legitimate conversation."],
    )


class NoulQuestion(BaseModel):
    """A yes/no question or statement, answered with the probability of yes or true."""

    type: Literal["noul"] = Field(description="Identifies a yes/no question or statement.")
    instructions: Instructions = Field(
        default=None,
        description="The yes/no question or statement to evaluate.",
        examples=["Is this message spam?"],
    )
    criteria: NoulCriteria | None = Field(
        default=None,
        description="Criteria clarifying what counts as a yes or no answer.",
    )


class ChoiceQuestion(BaseModel):
    """A question that selects one option from the choices you define."""

    type: Literal["choice"] = Field(
        description="Identifies a question that selects one of the choices in criteria."
    )
    instructions: Instructions = Field(
        default=None,
        description="What the model should decide when choosing an option.",
        examples=["What is the tone of this message?"],
    )
    criteria: dict[ChoiceName, Instructions] = Field(
        min_length=1,
        max_length=MAX_CHOICES,
        description=(
            "Choice names and descriptions of when each applies. "
            "A choice without a description is interpreted by its name alone."
        ),
        examples=[{"angry": "An upset or hostile message", "calm": "A neutral or polite message"}],
    )


class ScoreQuestion(BaseModel):
    """A question that assigns a score using an ordered rubric."""

    type: Literal["score"] = Field(
        description="Identifies a question that rates the content using the levels in criteria."
    )
    instructions: Instructions = Field(
        default=None,
        description="What the model should rate.",
        examples=["How urgent is this message?"],
    )
    criteria: list[Description] = Field(
        min_length=1,
        max_length=MAX_SCORE_LEVELS,
        description=(
            "Ordered descriptions of the score levels. "
            "Each description's position determines its score, starting at zero."
        ),
        examples=[["Can wait", "Needs attention this week", "Needs attention today"]],
    )


Question = Annotated[
    NoulQuestion | ChoiceQuestion | ScoreQuestion,
    Field(discriminator="type", description="A question about the supplied content."),
]


class SystemOneRequest(BaseModel):
    """Content and named questions to evaluate together."""

    state: str | dict[str, Any] | list[Any] = Field(
        description="The content all questions in this request refer to.",
        examples=["I was charged twice. Please help."],
    )
    model: str = Field(
        description="Name or alias of the model to use. Available names are returned by GET /v1/models.",
        examples=["jev-latest"],
    )
    questions: dict[QuestionName, Question] = Field(
        min_length=1,
        max_length=MAX_QUESTIONS,
        description=(
            "Questions to ask about the content, each with a name you choose. "
            "The response uses those names to identify the answers."
        ),
        examples=[{"billing": {"instructions": "Is this message about billing?", "type": "noul"}}],
    )


# --- Answers -----------------------------------------------------------------


class NoulAnswer(BaseModel):
    """The probability of a yes answer or a true statement."""

    type: Literal["noul"] = Field(description="Identifies a yes/no answer.")
    noul: float = Field(
        description="Probability of a yes answer or a true statement, from 0 to 1.",
        examples=[0.98],
    )


class ChoiceAnswer(BaseModel):
    """The selected choice, confidence, and probabilities for a choice question."""

    type: Literal["choice"] = Field(description="Identifies a selection from the requested choices.")
    choice: str = Field(description="The name of the choice with the highest probability.")
    confidence: float = Field(description="Confidence in the selected choice, from 0 to 1.")
    probabilities: dict[str, float] = Field(
        description="Probability of each choice in criteria, keyed by choice name. Values sum to approximately 1."
    )


class ScoreAnswer(BaseModel):
    """An expected score with its rubric, confidence, and score-level probabilities."""

    type: Literal["score"] = Field(description="Identifies a rating against the requested score levels.")
    score: float = Field(
        description="Expected score: the probability-weighted average of the rubric levels."
    )
    confidence: float = Field(description="Confidence in the score, from 0 to 1.")
    legend: dict[str, Description] = Field(
        description="The requested criteria mapped to their score levels, so you can interpret the score."
    )
    probabilities: dict[str, float] = Field(
        description="Probability of each score level, using the same keys as legend."
    )


Answer = Annotated[
    NoulAnswer | ScoreAnswer | ChoiceAnswer,
    Field(discriminator="type", description="An answer whose type matches the corresponding question."),
]


class Usage(BaseModel):
    """Token usage for the request. Synthetic estimates in the sandbox."""

    input_tokens: int = Field(description="Number of billable input tokens used to evaluate the request.")
    output_tokens: int = Field(description="Number of output tokens used to answer the questions.")


class SystemOneResponse(BaseModel):
    """Answers grouped by question name, with the model used and token usage."""

    model: str = Field(description="Name of the model that answered the questions.")
    answers: dict[str, Answer] = Field(
        min_length=1,
        description="Answers keyed by the question names supplied in the request.",
    )
    usage: Usage


class ModelMetadata(BaseModel):
    """A model or model alias available to the caller."""

    name: str = Field(description="Model name or alias accepted by the request's model field.")
    description: str = Field(description="Human-readable description of the model.")
    release_date: str = Field(description="Model release date, formatted as YYYY-MM-DD.")


class ModelMetadataList(BaseModel):
    """Models and aliases available to the caller."""

    models: list[ModelMetadata]
