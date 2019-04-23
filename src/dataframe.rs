use crate::table::Column;
use crate::utils;
use arrow::array;
use arrow::array::{Array, ArrayRef};
use arrow::array_data::ArrayDataBuilder;
use arrow::array_data::ArrayDataRef;
use arrow::csv::Reader as CsvReader;
use arrow::csv::ReaderBuilder as CsvReaderBuilder;
use arrow::datatypes::*;
use arrow::error::ArrowError;
use arrow::json::Reader as JsonReader;
use arrow::json::ReaderBuilder as JsonReaderBuilder;
use arrow::record_batch::RecordBatch;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

use crate::error::DataFrameError;

pub struct DataFrame {
    schema: Arc<Schema>,
    columns: Vec<Column>,
}

impl DataFrame {
    /// Create an empty `DataFrame`
    pub fn empty() -> Self {
        DataFrame {
            schema: Arc::new(Schema::empty()),
            columns: vec![],
        }
    }

    fn new(schema: Arc<Schema>, columns: Vec<Column>) -> Self {
        DataFrame { schema, columns }
    }

    fn from_arrays(schema: Arc<Schema>, arrays: Vec<Arc<Array>>) -> Self {
        let columns = arrays
            .into_iter()
            .enumerate()
            .map(|(i, array)| Column::from_arrays(vec![array], schema.field(i).clone()))
            .collect();
        DataFrame { schema, columns }
    }

    pub fn from_columns(schema: Arc<Schema>, columns: Vec<Column>) -> Self {
        DataFrame { schema, columns }
    }

    pub fn schema(&self) -> &Arc<Schema> {
        &self.schema
    }

    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    pub fn num_chunks(&self) -> usize {
        self.column(0).data.num_chunks()
    }

    pub fn num_rows(&self) -> usize {
        self.columns[0].data.num_rows()
    }

    /// Get a column from schema by index.
    pub fn column(&self, i: usize) -> &Column {
        &self.columns[i]
    }

    pub fn column_by_name(&self, name: &str) -> &Column {
        let column_number = self.schema.column_with_name(name).unwrap();
        let column = &self.columns[column_number.0];
        let field = self.schema.field(column_number.0);
        column
    }

    /// Returns a new `DataFrame` with column appended.
    pub fn with_column(mut self, name: &str, column: Column) -> Self {
        let mut fields = self.schema.fields().clone();
        // check if field exists, and overwrite it
        let col = self.schema.column_with_name(name);
        match col {
            Some((i, field)) => {
                self = self.drop(vec![name]);
                fields = self.schema.fields().clone();
            }
            None => {}
        }
        fields.push(Field::new(
            name,
            column.data_type().clone(),
            column.field().is_nullable(),
        ));
        self.schema = Arc::new(Schema::new(fields));
        self.columns.push(column);
        self
    }

    /// Returns the `DataFrame` with the specified column renamed
    pub fn with_column_renamed(mut self, old_name: &str, new_name: &str) -> Self {
        // this should only modify the schema
        let (index, field) = self.schema.column_with_name(old_name).unwrap();
        let new_field = Field::new(new_name, field.data_type().clone(), field.is_nullable());
        let mut fields = self.schema.fields().clone();
        fields[index] = new_field;
        self.schema = Arc::new(Schema::new(fields.to_vec()));
        self
    }

    /// Returns dataframe as an Arrow `RecordBatch`
    /// TODO: add a method to break into smaller batches
    fn to_record_batches(&self) -> Vec<RecordBatch> {
        let num_chunks = self.column(0).data().num_chunks();
        let num_columns = self.num_columns();
        let mut batches: Vec<RecordBatch> = Vec::with_capacity(num_chunks);
        let mut arrays: Vec<Vec<ArrayRef>> = Vec::with_capacity(num_chunks);
        // for i in 0..self.num_columns() {
        //     let column = self.column(i);
        //     if i == 0 {
        //         arrays.push(vec![]);
        //     }
        //     for j in 0..column.data().num_chunks() {
        //         arrays[i].push(column.data().chunk(j).to_owned());
        //     }
        // }

        for i in 0..num_chunks {
            let mut arr = vec![];

            // if i == 0 {
            //     arrays.push(vec![]);
            // }
            for j in 0..num_columns {
                let column = self.column(j);
                arr.push(column.data().chunk(i).to_owned());
            }

            arrays.push(arr);
        }

        arrays.into_iter().for_each(|array| {
            // the unwrap is infallible as we're passing data that's already been verified
            batches.push(RecordBatch::try_new(self.schema.clone(), array).unwrap());
        });

        batches
    }

    /// Returns dataframe with the first n records selected
    fn take(&self, count: usize) -> Self {
        DataFrame::new(
            self.schema.clone(),
            self.columns
                .clone()
                .into_iter()
                .map(|col| col.slice(0, Some(count)))
                .collect(),
        )
    }

    fn intersect(&self, other: &DataFrame) -> Self {
        unimplemented!("Intersect not yet implemented")
    }

    /// Returns dataframe with specified columns selected.
    ///
    /// If a column name does not exist, it is omitted.
    pub fn select(&mut self, col_names: Vec<&str>) -> Self {
        // get the names of columns from the schema, and match them with supplied
        let mut col_num: i16 = -1;
        let schema = &self.schema.clone();
        let field_names: Vec<(usize, &str)> = schema
            .fields()
            .iter()
            .map(|c| {
                col_num += 1;
                (col_num as usize, c.name().as_str())
            })
            .collect();

        // filter names
        let filter_cols: Vec<(usize, &str)> = if col_names.contains(&"*") {
            field_names
        } else {
            // TODO follow the order of user-supplied column names
            field_names
                .into_iter()
                .filter(|(col, name)| col_names.contains(name))
                .collect()
        };

        let mut columns = vec![];

        for (i, u) in filter_cols.clone() {
            let c = &self.columns[i];
            columns.push(c.clone());
        }

        let new_schema = Arc::new(Schema::new(
            filter_cols
                .iter()
                .map(|c| schema.field(c.0).clone())
                .collect(),
        ));

        DataFrame::from_columns(new_schema, columns)
    }

    /// Returns a dataframe with specified columns dropped.
    ///
    /// If a column name does not exist, it is omitted.
    pub fn drop(&self, col_names: Vec<&str>) -> Self {
        // get the names of columns from the schema, and match them with supplied
        let mut col_num: i16 = -1;
        let schema = self.schema.clone();
        let field_names: Vec<(usize, &str)> = schema
            .fields()
            .into_iter()
            .map(|c| {
                col_num += 1;
                (col_num as usize, c.name().as_str())
            })
            .collect();

        // filter names
        let filter_cols: Vec<(usize, &str)> = {
            // TODO follow the order of user-supplied column names
            field_names
                .into_iter()
                .filter(|(col, name)| !col_names.contains(name))
                .collect()
        };

        // construct dataframe with selected columns
        DataFrame {
            schema: Arc::new(Schema::new(
                filter_cols
                    .iter()
                    .map(|c| schema.field(c.0).clone())
                    .collect(),
            )),
            columns: filter_cols
                .iter()
                .map(move |c| self.columns[c.0].clone())
                .collect(),
        }
    }

    /// Create a dataframe from an Arrow Table.
    ///
    /// Arrow Tables are not yet in the Rust library, and we are hashing them out here
    pub fn from_table(table: crate::table::Table) -> Self {
        DataFrame {
            schema: table.schema().clone(),
            columns: table.columns().to_vec(),
        }
    }

    pub fn from_csv(path: &str, schema: Option<Arc<Schema>>) -> Self {
        let file = File::open(path).unwrap();
        let mut reader = match schema {
            Some(schema) => CsvReader::new(file, schema, true, 1024, None),
            None => {
                let builder = CsvReaderBuilder::new()
                    .infer_schema(None)
                    .has_headers(true)
                    .with_batch_size(1024);
                builder.build(file).unwrap()
            }
        };
        let mut batches: Vec<RecordBatch> = vec![];
        let mut has_next = true;
        while has_next {
            match reader.next() {
                Ok(batch) => match batch {
                    Some(batch) => {
                        batches.push(batch);
                    }
                    None => {
                        has_next = false;
                    }
                },
                Err(e) => {
                    has_next = false;
                }
            }
        }

        let schema: Arc<Schema> = batches[0].schema().clone();

        // convert to an arrow table
        let table = crate::table::Table::from_record_batches(schema.clone(), batches);

        // DataFrame::from_table(table)
        DataFrame {
            schema,
            columns: table.columns,
        }
    }

    pub fn from_feather(path: &str) -> Result<Self, ArrowError> {
        use crate::io::feather::*;

        let mut reader = FeatherReader::new(File::open(path)?);
        let batch = reader.read()?;

        let schema = batch.schema().clone();

        let table = crate::table::Table::from_record_batches(schema.clone(), vec![batch]);

        Ok(DataFrame {
            schema,
            columns: table.columns,
        })
    }

    pub fn from_json(path: &str, schema: Option<Arc<Schema>>) -> Self {
        let file = File::open(path).unwrap();
        let mut reader = match schema {
            Some(schema) => JsonReader::new(BufReader::new(file), schema, 1024, None),
            None => {
                let builder = JsonReaderBuilder::new()
                    .infer_schema(None)
                    .with_batch_size(1024);
                builder.build::<File>(file).unwrap()
            }
        };
        let mut batches: Vec<RecordBatch> = vec![];
        let mut has_next = true;
        while has_next {
            match reader.next() {
                Ok(batch) => match batch {
                    Some(batch) => {
                        batches.push(batch);
                    }
                    None => {
                        has_next = false;
                    }
                },
                Err(e) => {
                    has_next = false;
                }
            }
        }

        let schema: Arc<Schema> = batches[0].schema().clone();

        // convert to an arrow table
        let table = crate::table::Table::from_record_batches(schema.clone(), batches);

        // DataFrame::from_table(table)
        DataFrame {
            schema,
            columns: table.columns,
        }
    }

    /// Create a DataFrame from a SQL table
    ///
    /// Note: Only PostgreSQL is currently supported, and data is buffered in-memory before creating dataframe.
    /// This might be undesirable if reading large tables. However, note that this library currently performs
    /// eager evaluation, so the DataFrame would still be created and held in-memory. We will improve this with
    /// a better execution model in future.
    pub fn from_sql(connection_string: &str, table_name: &str) -> Self {
        let batches =
            crate::io::postgres::read_table(connection_string, table_name, 0, 1024).unwrap();
        if batches.is_empty() {
            DataFrame::empty()
        } else {
            let schema = batches.get(0).unwrap().schema().clone();
            let table = crate::table::Table::from_record_batches(schema.clone(), batches);
            DataFrame {
                schema,
                columns: table.columns,
            }
        }
    }

    /// Write dataframe to a feather file
    ///
    /// Data is currently written as individual batches (as Arrow doesn't yet support slicing).
    /// This will be rectified when the above condition is met.
    pub fn to_feather(&self, path: &str) -> Result<(), ArrowError> {
        use crate::io::feather::*;

        let record_batches = self.to_record_batches();

        record_batches.iter().enumerate().for_each(|(i, batch)| {
            let mut file_name = String::new();
            file_name.push_str(path);
            file_name.push_str("_");
            file_name.push_str(&i.to_string());
            file_name.push_str(".feather");
            batch.write_feather(&file_name).unwrap();
        });

        Ok(())
    }

    pub fn to_csv(&self, path: &str) -> Result<(), arrow::error::ArrowError> {
        // use csv::error::Error;
        use arrow::csv::Writer;

        let file = File::create(path)?;

        let wrt = Writer::new(file);

        let batches = self.to_record_batches();
        let batches_ref: Vec<&RecordBatch> = batches.iter().map(|b| b).collect();

        wrt.write(batches_ref)?;

        Ok(())
    }

    /// get a column from table as column expression
    pub fn expr_column(&self, i: usize) -> crate::expression::Column {
        self.schema().field(i).clone().into()
    }

    pub fn expr_column_by_name(&self, name: &str) -> crate::expression::Column {
        self.schema()
            .column_with_name(name)
            .unwrap()
            .1
            .clone()
            .into()
    }
}

mod tests {
    use crate::dataframe::DataFrame;
    use crate::functions::scalar::ScalarFunctions;
    use crate::table::*;
    use arrow::array::*;
    use arrow::datatypes::{DataType, Field, Float64Type};
    use std::sync::Arc;

    #[test]
    fn create_empty_dataframe() {
        let dataframe = DataFrame::empty();

        assert_eq!(0, dataframe.num_columns());
        assert_eq!(0, dataframe.schema().fields().len());
    }

    #[test]
    fn read_csv_to_dataframe() {
        let dataframe = DataFrame::from_csv("./test/data/uk_cities_with_headers.csv", None);

        assert_eq!(3, dataframe.num_columns());
        assert_eq!(37, dataframe.num_rows());
    }

    #[test]
    fn read_postgres_table_to_dataframe() {
        // table created with:
        // create table arrow_data
        // (
        //     int32 integer,
        //     bool boolean,
        //     int64 bigint,
        //     string varchar(255),
        //     timestamp timestamp,
        //     time time
        // );
        let dataframe = DataFrame::from_sql(
            "postgres://postgres:password@localhost:5432/postgres",
            "public.arrow_data",
        );
        assert_eq!(6, dataframe.num_columns());
        assert_eq!(1, dataframe.num_rows());

        let col_1 = dataframe.column(0);
        let col_2 = dataframe.column(1);
        let col_3 = dataframe.column(2);
        let col_4 = dataframe.column(3);
        let col_5 = dataframe.column(4);
        let col_6 = dataframe.column(5);
        assert_eq!(
            "PrimitiveArray<Int32>\n[\n  1,\n]",
            format!("{:?}", Int32Array::from(col_1.data().chunks()[0].data()))
        );
        assert_eq!(
            "PrimitiveArray<Boolean>\n[\n  true,\n]",
            format!("{:?}", BooleanArray::from(col_2.data().chunks()[0].data()))
        );
        assert_eq!(
            "PrimitiveArray<Int64>\n[\n  12345676354674,\n]",
            format!("{:?}", Int64Array::from(col_3.data().chunks()[0].data()))
        );
        assert_eq!(
            "lorem ipsum",
            std::str::from_utf8(BinaryArray::from(col_4.data().chunks()[0].data()).value(0))
                .unwrap()
        );
        assert_eq!(
            "PrimitiveArray<Timestamp(Millisecond)>\n[\n  2019-04-19T10:41:13.591,\n]",
            format!(
                "{:?}",
                TimestampMillisecondArray::from(col_5.data().chunks()[0].data())
            )
        );
        assert_eq!(
            "PrimitiveArray<Time64(Microsecond)>\n[\n  12:45:00,\n]",
            format!(
                "{:?}",
                Time64MicrosecondArray::from(col_6.data().chunks()[0].data())
            )
        );
    }

    #[test]
    fn dataframe_ops() {
        let mut dataframe = DataFrame::from_csv("./test/data/uk_cities_with_headers.csv", None);
        let a = dataframe.column_by_name("lat");
        let b = dataframe.column_by_name("lng");
        let sum = ScalarFunctions::add(
            col_to_prim_arrays::<Float64Type>(a),
            col_to_prim_arrays::<Float64Type>(b),
        );
        // TODO, make this better
        let sum: Vec<ArrayRef> = sum
            .unwrap()
            .into_iter()
            .map(|p| Arc::new(p) as ArrayRef)
            .collect();
        dataframe = dataframe.with_column(
            "lat_lng_sum",
            Column::from_arrays(sum, Field::new("lat_lng_sum", DataType::Float64, true)),
        );

        assert_eq!(4, dataframe.num_columns());
        assert_eq!(4, dataframe.schema().fields().len());
        assert_eq!(
            54.31776,
            col_to_prim_arrays::<Float64Type>(dataframe.column_by_name("lat_lng_sum"))[0].value(0)
        );

        dataframe = dataframe.with_column_renamed("lat_lng_sum", "ll_sum");

        assert_eq!("ll_sum", dataframe.schema().field(3).name());

        // dataframe = dataframe.select(vec!["*"]);

        // assert_eq!(4, dataframe.num_columns());
        // assert_eq!(4, dataframe.schema().fields().len());

        // let df2 = dataframe.select(vec!["lat", "lng"]);

        // assert_eq!(2, df2.num_columns());
        // assert_eq!(2, df2.schema().fields().len());

        // // drop columns from `dataframe`
        // let df3 = dataframe.drop(vec!["city", "ll_sum"]);

        // assert_eq!(df2.schema().fields(), df3.schema().fields());

        // calculate absolute value of `lng`
        let abs = ScalarFunctions::abs(col_to_prim_arrays::<Float64Type>(
            dataframe.column_by_name("lng"),
        ))
        .unwrap();

        assert_eq!(3.335724, abs[0].value(0));
    }

    #[test]
    fn feather_io() {
        let mut dataframe = DataFrame::from_csv("./test/data/uk_cities_with_headers.csv", None);
        let a = dataframe.column_by_name("lat");
        let b = dataframe.column_by_name("lng");
        let sum = ScalarFunctions::add(
            col_to_prim_arrays::<Float64Type>(a),
            col_to_prim_arrays::<Float64Type>(b),
        );
        // TODO, make this better
        let sum: Vec<ArrayRef> = sum
            .unwrap()
            .into_iter()
            .map(|p| Arc::new(p) as ArrayRef)
            .collect();
        dataframe = dataframe.with_column(
            "lat_lng_sum",
            Column::from_arrays(sum, Field::new("lat_lng_sum", DataType::Float64, true)),
        );

        let city = dataframe.column_by_name("city");
        let lowercase = ScalarFunctions::lower(col_to_binary_arrays(city));
        let lowercase: Vec<ArrayRef> = lowercase
            .unwrap()
            .into_iter()
            .map(|p| Arc::new(p) as ArrayRef)
            .collect();
        dataframe = dataframe.with_column(
            "city_lower",
            Column::from_arrays(lowercase, Field::new("city_lower", DataType::Utf8, true)),
        );

        let write = dataframe.to_feather("./test/data/uk_cities");
        assert!(write.is_ok());
    }

    #[test]
    fn csv_io() {
        let mut dataframe = DataFrame::from_csv("./test/data/uk_cities_with_headers.csv", None);
        let a = dataframe.column_by_name("lat");
        let b = dataframe.column_by_name("lng");
        let sum = ScalarFunctions::add(
            col_to_prim_arrays::<Float64Type>(a),
            col_to_prim_arrays::<Float64Type>(b),
        );
        // TODO, make this better
        let sum: Vec<ArrayRef> = sum
            .unwrap()
            .into_iter()
            .map(|p| Arc::new(p) as ArrayRef)
            .collect();
        dataframe = dataframe.with_column(
            "lat_lng_sum",
            Column::from_arrays(sum, Field::new("lat_lng_sum", DataType::Float64, true)),
        );

        let city = dataframe.column_by_name("city");
        let lowercase = ScalarFunctions::lower(col_to_binary_arrays(city));
        let lowercase: Vec<ArrayRef> = lowercase
            .unwrap()
            .into_iter()
            .map(|p| Arc::new(p) as ArrayRef)
            .collect();
        dataframe = dataframe.with_column(
            "city_lower",
            Column::from_arrays(lowercase, Field::new("city_lower", DataType::Utf8, true)),
        );

        let write = dataframe.to_csv("/tmp/uk_cities_out.csv");
        assert!(write.is_ok());
    }
}
