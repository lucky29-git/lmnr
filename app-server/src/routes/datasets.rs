use std::{collections::HashMap, sync::Arc};

use actix_multipart::Multipart;
use actix_web::{delete, get, post, web, HttpResponse};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    datasets::{datapoints, utils::read_multipart_file},
    db::{self, datapoints::DatapointView, datasets, DB},
    routes::{PaginatedGetQueryParams, PaginatedResponse, ResponseResult},
    semantic_search::SemanticSearch,
};

const DEFAULT_PAGE_SIZE: usize = 50;

#[delete("datasets/{dataset_id}")]
async fn delete_dataset(
    db: web::Data<DB>,
    path: web::Path<(Uuid, Uuid)>,
    semantic_search: web::Data<Arc<dyn SemanticSearch>>,
) -> ResponseResult {
    let (project_id, dataset_id) = path.into_inner();

    datasets::delete_dataset(&db.pool, dataset_id).await?;

    semantic_search
        .delete_embeddings(
            &project_id.to_string(),
            vec![HashMap::from([(
                "datasource_id".to_string(),
                dataset_id.to_string(),
            )])],
        )
        .await?;

    Ok(HttpResponse::Ok().finish())
}

// NOTE: this endpoint currently assumes one file upload.
// If we want to support multiple files, we will need to keep a list of filename -> bytes links.
// and potentially batch process, so that we don't hold enormous files in memory
#[post("datasets/{dataset_id}/file-upload")]
async fn upload_datapoint_file(
    payload: Multipart,
    path: web::Path<(Uuid, Uuid)>,
    db: web::Data<DB>,
    semantic_search: web::Data<Arc<dyn SemanticSearch>>,
) -> ResponseResult {
    let (project_id, dataset_id) = path.into_inner();
    let db = db.into_inner();

    let (filename, is_unstructured_file, bytes) = read_multipart_file(payload).await?;

    let dataset = db::datasets::get_dataset(&db.pool, project_id, dataset_id).await?;

    let mut indexed_on = dataset.indexed_on.clone();
    if indexed_on.is_none() && is_unstructured_file {
        // For user convenience, we will automatically index by content, if the dataset is empty and the file is unstructured
        let total_entries = db::datapoints::count_datapoints(&db.pool, dataset_id).await?;
        if total_entries == 0 {
            indexed_on = Some("content".to_string());
            db::datasets::update_index_column(&db.pool, dataset_id, indexed_on.clone()).await?;
        }
    }

    let datapoints =
        datapoints::insert_datapoints_from_file(&bytes, &filename, dataset_id, db.clone()).await?;

    if indexed_on.is_some() {
        dataset
            .index_new_points(
                datapoints.clone(),
                semantic_search.as_ref().clone(),
                project_id.to_string(),
                indexed_on,
            )
            .await?;
    }

    Ok(HttpResponse::Ok().json(datapoints))
}

#[derive(Deserialize)]
struct CreateDatapointsRequest {
    datapoints: Vec<serde_json::Value>,
}

#[post("datasets/{dataset_id}/datapoints")]
async fn create_datapoints(
    path: web::Path<(Uuid, Uuid)>,
    db: web::Data<DB>,
    req: web::Json<CreateDatapointsRequest>,
    semantic_search: web::Data<Arc<dyn SemanticSearch>>,
) -> ResponseResult {
    let (project_id, dataset_id) = path.into_inner();
    let input_datapoints = req.into_inner().datapoints;

    let dataset = db::datasets::get_dataset(&db.pool, project_id, dataset_id).await?;

    let datapoints =
        db::datapoints::insert_raw_data(&db.pool, &dataset_id, &input_datapoints).await?;

    if dataset.indexed_on.is_some() {
        dataset
            .index_new_points(
                datapoints.clone(),
                semantic_search.as_ref().clone(),
                project_id.to_string(),
                dataset.indexed_on.clone(),
            )
            .await?;
    }

    Ok(HttpResponse::Ok().json(datapoints))
}

#[derive(Deserialize)]
struct UpdateDatapointRequest {
    data: Value,
    target: Value,
    metadata: Option<Value>,
}

#[post("datasets/{dataset_id}/datapoints/{datapoint_id}")]
async fn update_datapoint_data(
    path: web::Path<(Uuid, Uuid, Uuid)>,
    db: web::Data<DB>,
    req: web::Json<UpdateDatapointRequest>,
    semantic_search: web::Data<Arc<dyn SemanticSearch>>,
) -> ResponseResult {
    let (project_id, dataset_id, datapoint_id) = path.into_inner();
    let req = req.into_inner();

    let updated_datapoint = db::datapoints::update_datapoint(
        &db.pool,
        &datapoint_id,
        &req.data,
        &req.target,
        &req.metadata,
    )
    .await?;

    let dataset = db::datasets::get_dataset(&db.pool, project_id, dataset_id).await?;
    if dataset.indexed_on.is_some() {
        dataset
            .index_new_points(
                vec![updated_datapoint.clone()],
                semantic_search.as_ref().clone(),
                project_id.to_string(),
                dataset.indexed_on.clone(),
            )
            .await?;
    }

    Ok(HttpResponse::Ok().json(updated_datapoint))
}

#[derive(Deserialize)]
pub struct DeleteDatapointRequest {
    pub ids: Vec<Uuid>,
}

#[delete("datasets/{dataset_id}/datapoints")]
async fn delete_datapoints(
    path: web::Path<(Uuid, Uuid)>,
    db: web::Data<DB>,
    req: web::Json<DeleteDatapointRequest>,
    semantic_search: web::Data<Arc<dyn SemanticSearch>>,
) -> ResponseResult {
    let (project_id, _dataset_id) = path.into_inner();
    let datapoint_ids = req.into_inner().ids;

    db::datapoints::delete_datapoints(&db.pool, &datapoint_ids).await?;

    semantic_search
        .delete_embeddings(
            &project_id.to_string(),
            datapoint_ids
                .iter()
                .map(|id| HashMap::from([("id".to_string(), id.to_string())]))
                .collect::<Vec<_>>(),
        )
        .await?;

    Ok(HttpResponse::Ok().finish())
}

#[delete("datasets/{dataset_id}/datapoints/all")]
async fn delete_all_datapoints(
    path: web::Path<(Uuid, Uuid)>,
    db: web::Data<DB>,
    semantic_search: web::Data<Arc<dyn SemanticSearch>>,
) -> ResponseResult {
    let (project_id, dataset_id) = path.into_inner();

    let deleted_dp_ids = db::datapoints::delete_all_datapoints(&db.pool, &dataset_id).await?;

    semantic_search
        .delete_embeddings(
            &project_id.to_string(),
            deleted_dp_ids
                .iter()
                .map(|id| HashMap::from([("id".to_string(), id.to_string())]))
                .collect::<Vec<_>>(),
        )
        .await?;

    Ok(HttpResponse::Ok().finish())
}

#[get("datasets/{dataset_id}/datapoints")]
async fn get_datapoints(
    db: web::Data<DB>,
    path: web::Path<(Uuid, Uuid)>,
    query_params: web::Query<PaginatedGetQueryParams>,
) -> ResponseResult {
    let (_project_id, dataset_id) = path.into_inner();
    let limit = query_params.page_size.unwrap_or(DEFAULT_PAGE_SIZE) as i64;
    let offset = limit * (query_params.page_number) as i64;
    let datapoints = db::datapoints::get_datapoints(&db.pool, dataset_id, limit, offset).await?;
    let total_entries = db::datapoints::count_datapoints(&db.pool, dataset_id).await?;

    let response = PaginatedResponse::<DatapointView> {
        items: datapoints,
        total_count: total_entries,
        any_in_project: true,
    };

    Ok(HttpResponse::Ok().json(response))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexDatasetRequest {
    index_column: Option<String>,
}

#[post("datasets/{dataset_id}/index")]
async fn index_dataset(
    db: web::Data<DB>,
    path: web::Path<(Uuid, Uuid)>,
    request: web::Json<IndexDatasetRequest>,
    semantic_search: web::Data<Arc<dyn SemanticSearch>>,
) -> ResponseResult {
    let (project_id, dataset_id) = path.into_inner();
    let index_column = &request.index_column;
    let dataset = db::datasets::get_dataset(&db.pool, project_id, dataset_id).await?;

    if &dataset.indexed_on == index_column {
        return Ok(HttpResponse::Ok().json(dataset));
    }

    // TODO: batch process this in pages
    let datapoints = db::datapoints::get_all_datapoints(&db.pool, dataset_id).await?;
    // First, delete old embeddings
    if dataset.indexed_on.is_some() {
        semantic_search
            .delete_embeddings(
                &project_id.to_string(),
                vec![HashMap::from([(
                    "datasource_id".to_string(),
                    dataset_id.to_string(),
                )])],
            )
            .await?;
    }

    // Then, index all embeddings
    if index_column.is_some() {
        dataset
            .index_new_points(
                datapoints.clone(),
                semantic_search.as_ref().clone(),
                project_id.to_string(),
                index_column.clone(),
            )
            .await?;
    }

    let dataset =
        db::datasets::update_index_column(&db.pool, dataset_id, index_column.clone()).await?;

    Ok(HttpResponse::Ok().json(dataset))
}
