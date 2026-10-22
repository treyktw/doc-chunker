"""Regenerate the binary sample files in this directory.

    pip install python-docx python-pptx reportlab
    python samples/generate.py

Everything here is synthetic text about houseplant care. The .md and .html
samples are written by hand and checked in; this script only builds the
formats that cannot be written by hand: .docx, .pptx and .pdf.
"""

from datetime import datetime, timezone
from pathlib import Path

from docx import Document
from pptx import Presentation
from pptx.util import Inches, Pt
from reportlab.lib.pagesizes import A5
from reportlab.lib.styles import getSampleStyleSheet
from reportlab.platypus import PageBreak, Paragraph, SimpleDocTemplate, Spacer

HERE = Path(__file__).resolve().parent

WATERING = (
    "Most houseplants want the top few centimetres of soil to dry out between "
    "waterings. Pushing a finger into the pot is a better guide than a calendar, "
    "because light, temperature and pot size all change how fast soil dries."
)
OVERWATERING = (
    "Overwatering is the most common way houseplants die. Roots sitting in wet "
    "soil cannot take up oxygen, so they rot, and a plant with rotten roots wilts "
    "exactly like a plant that needs water."
)
LIGHT = (
    "Bright indirect light suits most foliage plants. A south-facing window can "
    "scorch thin leaves in summer, while a north-facing room may be too dim for "
    "anything except the most shade-tolerant species."
)
REPOTTING = (
    "Repot when roots circle the bottom of the pot or water runs straight through. "
    "Move up only one pot size at a time; a much larger pot holds water the roots "
    "cannot reach, which brings back the overwatering problem."
)
PESTS = (
    "Check the undersides of leaves for fungus gnats, mealybugs and spider mites. "
    "Isolating a new plant for two weeks keeps a hitchhiking pest from spreading "
    "to the rest of the collection."
)


FIXED_DATE = datetime(2026, 1, 1, tzinfo=timezone.utc)


def scrub_core_properties(properties, title: str) -> None:
    """Replace the library templates' default authorship and dates with
    obviously synthetic values, so the samples carry no real names and the
    bytes stay stable between runs."""
    properties.title = title
    properties.author = "Sample Author"
    properties.last_modified_by = "Sample Author"
    properties.comments = "Synthetic sample"
    properties.created = FIXED_DATE
    properties.modified = FIXED_DATE
    properties.revision = 1


def build_docx(path: Path) -> None:
    document = Document()
    scrub_core_properties(document.core_properties, "Houseplant Care Basics")
    document.add_heading("Houseplant Care Basics", level=1)
    document.add_heading("Watering", level=2)
    document.add_paragraph(WATERING)
    document.add_paragraph(OVERWATERING)
    document.add_heading("Light", level=2)
    document.add_paragraph(LIGHT)
    document.add_heading("Repotting", level=2)
    document.add_paragraph(REPOTTING)
    document.save(path)


def build_pptx(path: Path) -> None:
    presentation = Presentation()
    scrub_core_properties(presentation.core_properties, "Houseplant Care")
    slides = [
        ("Houseplant Care", "A five minute overview for new plant owners."),
        ("Watering", WATERING),
        ("Light", LIGHT),
        ("Pests", PESTS),
    ]
    layout = presentation.slide_layouts[1]
    for title, body in slides:
        slide = presentation.slides.add_slide(layout)
        slide.shapes.title.text = title
        slide.placeholders[1].text = body
    presentation.save(path)


def build_pdf(path: Path) -> None:
    styles = getSampleStyleSheet()
    story = [
        Paragraph("Houseplant Care Handbook", styles["Title"]),
        Paragraph("Watering", styles["Heading2"]),
        Paragraph(WATERING, styles["BodyText"]),
        Spacer(1, 8),
        Paragraph(OVERWATERING, styles["BodyText"]),
        PageBreak(),
        Paragraph("Light", styles["Heading2"]),
        Paragraph(LIGHT, styles["BodyText"]),
        PageBreak(),
        Paragraph("Repotting", styles["Heading2"]),
        Paragraph(REPOTTING, styles["BodyText"]),
        Spacer(1, 8),
        Paragraph(PESTS, styles["BodyText"]),
    ]
    SimpleDocTemplate(
        str(path),
        pagesize=A5,
        title="Houseplant Care Handbook",
        author="Sample Author",
        invariant=1,  # stable bytes: no creation date or random document id
    ).build(story)


if __name__ == "__main__":
    build_docx(HERE / "plant-care.docx")
    build_pptx(HERE / "plant-care.pptx")
    build_pdf(HERE / "plant-care.pdf")
    print("wrote plant-care.docx, plant-care.pptx, plant-care.pdf")
