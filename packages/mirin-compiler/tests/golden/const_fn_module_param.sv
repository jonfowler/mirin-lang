module sink #(parameter int W) (
    input  logic [W-1:0] x,
    output logic [7:0] result
);
    assign result = (8'(x));
endmodule

module top #(parameter int N) (
    input  logic [7:0] x,
    output logic [7:0] result
);
    function automatic int sum_to(input int n);
        int acc;
        acc = 0;
        for (int i = 0; i < n; i++) begin
            acc = (acc + i);
        end
        return acc;
    endfunction
    localparam int w = sum_to(N);
    logic [w-1:0] wide;
    assign wide = (w'(x));
    sink #(
        .W(w)
    ) sink (
        .x(wide),
        .result(result)
    );
endmodule
